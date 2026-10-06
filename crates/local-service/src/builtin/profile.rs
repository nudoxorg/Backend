//! Compiled profile identities, model, validator, and dispatcher.
//!
//! This module is the profile composition boundary. It owns the registered
//! relation recipe and its checked authority/semantic contracts; process and
//! transport state stay in the parent façade and the replication module.

use super::{
    AUTHORITY_VALUE, Arc, AuthorityVersionSchema, Blake3AuthorityVerifier, Budget, Commit,
    CommitProvenance, CompleteSemanticCoverage, CompositeAdmissionValidator, DependencyManifest,
    DispatchError, Dispatcher, ECHO_AUTHORITY_SECRET, LazyClosureUpdate, ObjectClosure, ObjectKey,
    ObjectVersion, OutputVersion, PreparedTransition, RelationTransition, RemoteAuthorityPolicy,
    ResourceVector, Scheduler, Schema, SemanticCoverageAdmissionError, SemanticCoverageBinding,
    TransactionId, TypedObject, UntrustedSemanticCoverageClaim, WorkspaceClosure, WorkspaceDelta,
    WorkspaceManifest, WorkspaceModel, WorkspaceSnapshot, admit_manifest, fmt,
    transition_closure_lazy, workspace_manifest_from_root,
};
#[cfg(test)]
use backend_engine::builtin::SemanticPublicationClaim;
use backend_engine::builtin::{
    ProductSemanticCaptureOutcome, ProductSemanticCaptureRecord, ProductSemanticCaptureRelation,
    ProductSemanticCaptureRootSchema, ProductSemanticPublicationKey,
    ProductSemanticPublicationRecord, ProductSemanticPublicationRelation,
    ProductSourceFileFactsRecord, ProductSourceFileFactsRelation, ProductSourceFileFactsRootSchema,
    SemanticPublicationVersion, SemanticSourceCapture, admit_product_source_file_facts,
    product_source_file_facts_record_key, product_source_file_facts_relation,
    product_source_file_facts_root_object, semantic_capture_relation, semantic_capture_root_object,
};
use backend_engine::{
    CanonicalRelation, LazyPreparedUpdate, Relation, SemanticCoverageValidator, TransitionWork,
    TreeChange, WorkspaceRelationHandle,
};

pub(super) use backend_engine::builtin::{
    ECHO_OUTPUT_BYTES, PRODUCT_OUTPUT_BYTES, ProductSourceRelation as ProductRelation,
    Profile as BuiltinProfile, ProfileDescriptor, ProfileIds, execution_manifest,
    execution_resources, product_dependency_manifest, profile_descriptor, profile_output_len,
};

/// Product source relation aliases keep locald as a thin composition layer.
pub(super) type BuiltinWorkspaceRelation = backend_engine::ProductSourceRelation;
pub(super) type BuiltinPackageRecord = backend_engine::ProductSourceRecord;
pub(super) type BuiltinSemanticRelation = ProductSemanticPublicationRelation;

/// Current admission and the read-only historical probe share the full
/// membership proof; only the exact source-file key derivation differs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceFileKeyLayout {
    Current,
    Retired,
}

impl SourceFileKeyLayout {
    fn admits(self, project: [u8; 32], path: &str, key: [u8; 32]) -> bool {
        let current = backend_engine::product_source_file_key(project, path);
        match self {
            Self::Current => key == current,
            Self::Retired => {
                key != current
                    && key == backend_engine::legacy_product_source_file_key(project, path)
            }
        }
    }
}

fn prepare_source_update(
    relation: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    project_key: [u8; 32],
    changes: &[BuiltinSourceChange],
) -> Result<LazyPreparedUpdate<BuiltinWorkspaceRelation>, BuiltinModelError> {
    prepare_source_update_with_layout(relation, project_key, changes, SourceFileKeyLayout::Current)
}

fn prepare_source_update_with_layout(
    relation: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    project_key: [u8; 32],
    changes: &[BuiltinSourceChange],
    layout: SourceFileKeyLayout,
) -> Result<LazyPreparedUpdate<BuiltinWorkspaceRelation>, BuiltinModelError> {
    validate_source_membership_transition(relation, project_key, changes, layout)?;
    let changes = changes
        .iter()
        .map(|change| TreeChange {
            key: change.key,
            after: change.after.clone(),
        })
        .collect::<Vec<_>>();
    relation
        .prepare_update(&changes)
        .map_err(|error| BuiltinModelError(format!("prepare product source delta: {error}")))
}

/// Checks one project and its complete file/page frontier against the base
/// relation overlaid with the proposed source changes. This runs both for a
/// live intent and for restart admission before a new relation root can be
/// published.
fn validate_source_membership_transition(
    relation: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    project_key: [u8; 32],
    changes: &[BuiltinSourceChange],
    layout: SourceFileKeyLayout,
) -> Result<(), BuiltinModelError> {
    if changes.is_empty() {
        return Ok(());
    }
    for change in changes {
        if let Some(record) = &change.after {
            if let Some(fields) = record.project_fields() {
                if change.key != project_key
                    || backend_engine::package_key(fields.label).to_bytes() != project_key
                {
                    return Err(BuiltinModelError(
                        "project source row key does not match its package coordinate".to_owned(),
                    ));
                }
            } else if let Some(fields) = record.file_fields() {
                if fields.project != project_key
                    || !layout.admits(project_key, fields.path, change.key)
                {
                    return Err(BuiltinModelError(
                        "source file key, path, and owning project disagree".to_owned(),
                    ));
                }
            } else if let Some(fields) = record.membership_page_fields() {
                let page_key_matches = backend_engine::product_source_membership_page_key(
                    fields.project,
                    fields.files,
                )
                .is_ok_and(|expected| expected == change.key);
                if fields.project != project_key || !page_key_matches {
                    return Err(BuiltinModelError(
                        "membership page key does not commit its project and contents".to_owned(),
                    ));
                }
            }
        }
    }

    let old_project = relation
        .lookup(&project_key)
        .map_err(|error| BuiltinModelError(format!("read prior project row: {error}")))?;
    if old_project
        .as_ref()
        .is_some_and(|record| record.project_fields().is_none())
    {
        return Err(BuiltinModelError(
            "project relation key contains a non-project source row".to_owned(),
        ));
    }
    if old_project
        .as_ref()
        .and_then(BuiltinPackageRecord::project_fields)
        .is_some_and(|fields| backend_engine::package_key(fields.label).to_bytes() != project_key)
    {
        return Err(BuiltinModelError(
            "prior project row does not match its package coordinate".to_owned(),
        ));
    }
    let new_project = lookup_after_changes(relation, changes, &project_key)?;
    if new_project
        .as_ref()
        .is_some_and(|record| record.project_fields().is_none())
    {
        return Err(BuiltinModelError(
            "project update resolves to a non-project source row".to_owned(),
        ));
    }
    if new_project
        .as_ref()
        .and_then(BuiltinPackageRecord::project_fields)
        .is_some_and(|fields| backend_engine::package_key(fields.label).to_bytes() != project_key)
    {
        return Err(BuiltinModelError(
            "updated project row does not match its package coordinate".to_owned(),
        ));
    }

    let old_files = match old_project.as_ref() {
        Some(record) => resolve_project_file_keys(project_key, record, |key| {
            relation
                .lookup(key)
                .map_err(|error| BuiltinModelError(format!("read prior membership page: {error}")))
        })?,
        None => Vec::new(),
    };
    let old_file_records = relation
        .lookup_many_sorted(&old_files)
        .map_err(|error| BuiltinModelError(format!("read prior project files: {error}")))?;
    for (key, record) in old_files.iter().copied().zip(old_file_records) {
        let record = record.ok_or_else(|| {
            BuiltinModelError("project frontier refers to a missing source file".to_owned())
        })?;
        validate_project_file_with_layout(project_key, key, &record, layout)?;
    }
    let new_files = match new_project.as_ref() {
        Some(record) => resolve_project_file_keys(project_key, record, |key| {
            lookup_after_changes(relation, changes, key)
        })?,
        None => Vec::new(),
    };
    let old_file_set = old_files
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let new_file_set = new_files
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();

    for file_key in old_file_set.difference(&new_file_set) {
        if !changes
            .binary_search_by_key(file_key, |change| change.key)
            .ok()
            .is_some_and(|index| changes[index].after.is_none())
        {
            return Err(BuiltinModelError(
                "project transition leaves a formerly selected file row behind".to_owned(),
            ));
        }
    }
    for change in changes {
        match &change.after {
            Some(record) if record.file_fields().is_some() => {
                if !new_file_set.contains(&change.key) {
                    return Err(BuiltinModelError(
                        "source update adds a file outside the selected project frontier"
                            .to_owned(),
                    ));
                }
            }
            None => {}
            Some(_) => {}
        }
    }

    let old_pages = old_project
        .as_ref()
        .and_then(BuiltinPackageRecord::project_fields)
        .map_or(&[][..], |fields| fields.files.page_keys());
    let new_pages = new_project
        .as_ref()
        .and_then(BuiltinPackageRecord::project_fields)
        .map_or(&[][..], |fields| fields.files.page_keys());
    let old_page_set = old_pages
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let new_page_set = new_pages
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let new_file_keys = new_files
        .iter()
        .copied()
        .filter(|key| !old_file_set.contains(key))
        .collect::<Vec<_>>();
    let new_base_rows = relation
        .lookup_many_sorted(&new_file_keys)
        .map_err(|error| BuiltinModelError(format!("check new project file keys: {error}")))?;
    for (key, before) in new_file_keys.iter().copied().zip(new_base_rows) {
        let after = changes
            .binary_search_by_key(&key, |change| change.key)
            .ok()
            .and_then(|index| changes[index].after.as_ref());
        match (after, before) {
            (Some(record), None) if record.file_fields().is_some() => {
                validate_project_file_with_layout(project_key, key, record, layout)?;
            }
            (Some(record), Some(_)) if record.file_fields().is_some() => {
                return Err(BuiltinModelError(
                    "new source file key collides with an unselected source row".to_owned(),
                ));
            }
            (Some(_), _) => {
                return Err(BuiltinModelError(
                    "project frontier file key resolves to a non-file update".to_owned(),
                ));
            }
            (None, Some(_)) => {
                return Err(BuiltinModelError(
                    "project transition adopts a source row outside its prior frontier".to_owned(),
                ));
            }
            (None, None) => {
                return Err(BuiltinModelError(
                    "project frontier refers to a missing source file".to_owned(),
                ));
            }
        }
    }
    for key in new_files.iter().filter(|key| old_file_set.contains(*key)) {
        if changes
            .binary_search_by_key(key, |change| change.key)
            .ok()
            .is_some_and(|index| {
                changes[index]
                    .after
                    .as_ref()
                    .is_none_or(|record| record.file_fields().is_none())
            })
        {
            return Err(BuiltinModelError(
                "retained project file key resolves to a non-file update".to_owned(),
            ));
        }
    }
    for change in changes {
        if change
            .after
            .as_ref()
            .and_then(BuiltinPackageRecord::membership_page_fields)
            .is_some()
            && !old_page_set.contains(&change.key)
            && lookup_before(relation, change.key)?.is_some()
        {
            return Err(BuiltinModelError(
                "new membership page key collides with an unselected source row".to_owned(),
            ));
        }
    }
    for change in changes.iter().filter(|change| change.after.is_none()) {
        let was_selected_project = change.key == project_key && old_project.is_some();
        if !was_selected_project
            && !old_file_set.contains(&change.key)
            && !old_page_set.contains(&change.key)
        {
            return Err(BuiltinModelError(
                "source update deletes a row outside its prior project frontier".to_owned(),
            ));
        }
    }
    for page_key in old_page_set.difference(&new_page_set) {
        if !changes
            .binary_search_by_key(page_key, |change| change.key)
            .ok()
            .is_some_and(|index| changes[index].after.is_none())
        {
            return Err(BuiltinModelError(
                "project transition leaves a superseded membership page behind".to_owned(),
            ));
        }
    }
    for change in changes {
        let after_page = change
            .after
            .as_ref()
            .and_then(BuiltinPackageRecord::membership_page_fields);
        if after_page.is_some() && !new_page_set.contains(&change.key) {
            return Err(BuiltinModelError(
                "source update adds an unreferenced membership page".to_owned(),
            ));
        }
    }
    Ok(())
}

fn lookup_before(
    relation: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    key: [u8; 32],
) -> Result<Option<BuiltinPackageRecord>, BuiltinModelError> {
    relation
        .lookup(&key)
        .map_err(|error| BuiltinModelError(format!("read prior source row: {error}")))
}

fn lookup_after_changes(
    relation: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    changes: &[BuiltinSourceChange],
    key: &[u8; 32],
) -> Result<Option<BuiltinPackageRecord>, BuiltinModelError> {
    if let Ok(index) = changes.binary_search_by_key(key, |change| change.key) {
        return Ok(changes[index].after.clone());
    }
    relation
        .lookup(key)
        .map_err(|error| BuiltinModelError(format!("read source row in proposed root: {error}")))
}

/// Resolves all ordered file keys after authenticating every referenced page.
/// File-row reads are left to the caller so a lazy relation can batch them.
pub(super) fn resolve_project_file_keys(
    project_key: [u8; 32],
    record: &BuiltinPackageRecord,
    mut lookup_page: impl FnMut(&[u8; 32]) -> Result<Option<BuiltinPackageRecord>, BuiltinModelError>,
) -> Result<Vec<[u8; 32]>, BuiltinModelError> {
    let fields = record.project_fields().ok_or_else(|| {
        BuiltinModelError("project frontier resolves to a non-project row".to_owned())
    })?;
    if backend_engine::package_key(fields.label).to_bytes() != project_key {
        return Err(BuiltinModelError(
            "project frontier label does not match its relation key".to_owned(),
        ));
    }
    let keys = fields
        .iter_file_keys(project_key, |key| {
            lookup_page(key).map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(BuiltinModelError)?;
    Ok(keys)
}

pub(super) fn validate_project_file(
    project_key: [u8; 32],
    file_key: [u8; 32],
    record: &BuiltinPackageRecord,
) -> Result<(), BuiltinModelError> {
    validate_project_file_with_layout(project_key, file_key, record, SourceFileKeyLayout::Current)
}

pub(super) fn validate_project_file_with_layout(
    project_key: [u8; 32],
    file_key: [u8; 32],
    record: &BuiltinPackageRecord,
    layout: SourceFileKeyLayout,
) -> Result<(), BuiltinModelError> {
    let fields = record.file_fields().ok_or_else(|| {
        BuiltinModelError("project frontier refers to a non-file source row".to_owned())
    })?;
    if fields.project != project_key || !layout.admits(project_key, fields.path, file_key) {
        return Err(BuiltinModelError(
            "project frontier file key, path, and owner disagree".to_owned(),
        ));
    }
    Ok(())
}

fn prepare_semantic_update(
    relation: &WorkspaceRelationHandle<BuiltinSemanticRelation>,
    changes: &[BuiltinSemanticChange],
) -> Result<LazyPreparedUpdate<BuiltinSemanticRelation>, BuiltinModelError> {
    let changes = changes
        .iter()
        .map(|change| TreeChange {
            key: change.key.clone(),
            after: change.after.clone(),
        })
        .collect::<Vec<_>>();
    relation
        .prepare_update(&changes)
        .map_err(|error| BuiltinModelError(format!("prepare semantic publication delta: {error}")))
}

/// One authoritative package intent carried through the workspace owner.
///
/// The canonical bytes are retained in the checked closure, so recovery can
/// reconstruct the exact package operation before rebuilding its relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinIntent {
    encoding_version: u8,
    pub(super) operation: BuiltinIntentOperation,
    pub(super) package: backend_engine::PackageKey,
    pub(super) label: String,
    changes: Box<[BuiltinSourceChange]>,
    semantic_changes: Box<[BuiltinSemanticChange]>,
    capture_changes: Box<[BuiltinCaptureChange]>,
    source_facts_changes: Box<[BuiltinSourceFactsChange]>,
    semantic_selection: Option<BuiltinSemanticSelectionIntent>,
    operation_key: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BuiltinSourceChange {
    pub(super) key: [u8; 32],
    pub(super) after: Option<BuiltinPackageRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BuiltinSemanticChange {
    pub(super) key: ProductSemanticPublicationKey,
    pub(super) after: Option<ProductSemanticPublicationRecord>,
}

/// Exact before-state and requested capture outcome for one profile marker.
/// The target workspace root and commit are filled by the model only after it
/// has derived the immutable source transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BuiltinCaptureChange {
    pub(super) key: ProductSemanticPublicationKey,
    pub(super) expected: Option<ProductSemanticCaptureRecord>,
    pub(super) capture: SemanticSourceCapture,
    pub(super) outcome: ProductSemanticCaptureOutcome,
    /// Optional closed compiler fault explaining a terminal unavailable result.
    pub(super) compiler_failure: Option<backend_library::PackageCompilerFailure>,
}

/// Exact before/after evidence for one complete source-facts relation row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BuiltinSourceFactsChange {
    pub(super) key: [u8; 32],
    pub(super) expected: Option<ProductSourceFileFactsRecord>,
    pub(super) after: Option<ProductSourceFileFactsRecord>,
}

/// Exact before/after evidence for changing the selected compiler generation.
///
/// The immutable history key proves which compiler publication was requested;
/// the selected key names the only mutable package/profile slot. Retaining the
/// previous value makes restart admission replay the same state transition
/// instead of interpreting a semantic upsert as an ordinary index operation.
#[derive(Clone, Debug, Eq, PartialEq)]
struct BuiltinSemanticSelectionIntent {
    selected: ProductSemanticPublicationKey,
    generation: ProductSemanticPublicationKey,
    before: Option<ProductSemanticPublicationRecord>,
    after: ProductSemanticPublicationRecord,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BuiltinIntentOperation {
    Add,
    Remove,
    Index,
    SelectSemanticGeneration,
}

impl BuiltinIntent {
    const VERSION: u8 = 4;
    const KEYED_VERSION: u8 = 5;
    const CAPTURE_VERSION: u8 = 6;
    const FACTS_VERSION: u8 = 7;
    const TYPED_FAILURE_VERSION: u8 = 8;
    const ADD: u8 = 1;
    const REMOVE: u8 = 2;
    const INDEX: u8 = 3;
    const SELECT_SEMANTIC_GENERATION: u8 = 4;
    const MAX_LABEL_BYTES: usize = 4096;

    /// Creates an intent that adds or replaces one package coordinate.
    ///
    /// # Errors
    ///
    /// Returns an error when the package label exceeds the bounded intent size.
    pub fn add(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
    ) -> Result<Self, BuiltinModelError> {
        let label = label.into();
        let record = BuiltinPackageRecord::new(label.clone()).map_err(BuiltinModelError)?;
        let semantic_changes = added_package_semantic_terminal(package, &label)?;
        Self::new(
            BuiltinIntentOperation::Add,
            package,
            label,
            vec![BuiltinSourceChange {
                key: package.to_bytes(),
                after: Some(record),
            }],
            semantic_changes,
            None,
        )
    }

    /// Creates an intent that removes one package coordinate.
    ///
    /// # Errors
    ///
    /// Returns an error when the package label exceeds the bounded intent size.
    pub fn remove(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
    ) -> Result<Self, BuiltinModelError> {
        Self::remove_project(package, label, &[], &[], Vec::new())
    }

    /// Creates an intent that removes a project and every file selected by
    /// its last admitted frontier.
    pub(super) fn remove_project(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        files: &[[u8; 32]],
        membership_pages: &[[u8; 32]],
        semantic_changes: Vec<BuiltinSemanticChange>,
    ) -> Result<Self, BuiltinModelError> {
        let label = label.into();
        let mut changes = Vec::with_capacity(
            files
                .len()
                .saturating_add(membership_pages.len())
                .saturating_add(1),
        );
        changes.push(BuiltinSourceChange {
            key: package.to_bytes(),
            after: None,
        });
        changes.extend(
            membership_pages
                .iter()
                .copied()
                .map(|key| BuiltinSourceChange { key, after: None }),
        );
        changes.extend(
            files
                .iter()
                .copied()
                .map(|key| BuiltinSourceChange { key, after: None }),
        );
        Self::new(
            BuiltinIntentOperation::Remove,
            package,
            label,
            changes,
            semantic_changes,
            None,
        )
    }

    pub(super) fn remove_project_with_semantics(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        files: &[[u8; 32]],
        membership_pages: &[[u8; 32]],
        semantic_changes: Vec<BuiltinSemanticChange>,
    ) -> Result<Self, BuiltinModelError> {
        let label = label.into();
        let mut changes = Vec::with_capacity(
            files
                .len()
                .saturating_add(membership_pages.len())
                .saturating_add(1),
        );
        changes.push(BuiltinSourceChange {
            key: package.to_bytes(),
            after: None,
        });
        changes.extend(
            membership_pages
                .iter()
                .copied()
                .map(|key| BuiltinSourceChange { key, after: None }),
        );
        changes.extend(
            files
                .iter()
                .copied()
                .map(|key| BuiltinSourceChange { key, after: None }),
        );
        Self::new(
            BuiltinIntentOperation::Remove,
            package,
            label,
            changes,
            semantic_changes,
            None,
        )
    }

    pub(super) fn index_with_semantics(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        changes: Vec<BuiltinSourceChange>,
        semantic_changes: Vec<BuiltinSemanticChange>,
    ) -> Result<Self, BuiltinModelError> {
        Self::new(
            BuiltinIntentOperation::Index,
            package,
            label,
            changes,
            semantic_changes,
            None,
        )
    }

    pub(super) fn index_with_capture(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        changes: Vec<BuiltinSourceChange>,
        semantic_changes: Vec<BuiltinSemanticChange>,
        capture_changes: Vec<BuiltinCaptureChange>,
    ) -> Result<Self, BuiltinModelError> {
        let version = if capture_changes
            .iter()
            .any(|change| change.compiler_failure.is_some())
        {
            Self::TYPED_FAILURE_VERSION
        } else {
            Self::CAPTURE_VERSION
        };
        Self::new_with_version(
            version,
            BuiltinIntentOperation::Index,
            package,
            label,
            changes,
            semantic_changes,
            None,
            None,
            capture_changes,
            Vec::new(),
        )
    }

    pub(super) fn with_source_facts(
        self,
        source_facts_changes: Vec<BuiltinSourceFactsChange>,
    ) -> Result<Self, BuiltinModelError> {
        let version = if self
            .capture_changes
            .iter()
            .any(|change| change.compiler_failure.is_some())
        {
            Self::TYPED_FAILURE_VERSION
        } else {
            Self::FACTS_VERSION
        };
        Self::new_with_version(
            version,
            self.operation,
            self.package,
            self.label,
            self.changes.into_vec(),
            self.semantic_changes.into_vec(),
            self.semantic_selection,
            self.operation_key,
            self.capture_changes.into_vec(),
            source_facts_changes,
        )
    }

    pub(super) fn index_with_source_facts(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        changes: Vec<BuiltinSourceChange>,
        semantic_changes: Vec<BuiltinSemanticChange>,
        capture_changes: Vec<BuiltinCaptureChange>,
        source_facts_changes: Vec<BuiltinSourceFactsChange>,
    ) -> Result<Self, BuiltinModelError> {
        let version = if capture_changes
            .iter()
            .any(|change| change.compiler_failure.is_some())
        {
            Self::TYPED_FAILURE_VERSION
        } else {
            Self::FACTS_VERSION
        };
        Self::new_with_version(
            version,
            BuiltinIntentOperation::Index,
            package,
            label,
            changes,
            semantic_changes,
            None,
            None,
            capture_changes,
            source_facts_changes,
        )
    }

    pub(super) fn select_semantic_generation(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        selected: ProductSemanticPublicationKey,
        generation: ProductSemanticPublicationKey,
        before: Option<ProductSemanticPublicationRecord>,
        after: ProductSemanticPublicationRecord,
    ) -> Result<Self, BuiltinModelError> {
        let selection = BuiltinSemanticSelectionIntent {
            selected: selected.clone(),
            generation,
            before,
            after: after.clone(),
        };
        Self::new(
            BuiltinIntentOperation::SelectSemanticGeneration,
            package,
            label,
            Vec::new(),
            vec![BuiltinSemanticChange {
                key: selected,
                after: Some(after),
            }],
            Some(selection),
        )
    }

    fn new(
        operation: BuiltinIntentOperation,
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        changes: Vec<BuiltinSourceChange>,
        semantic_changes: Vec<BuiltinSemanticChange>,
        semantic_selection: Option<BuiltinSemanticSelectionIntent>,
    ) -> Result<Self, BuiltinModelError> {
        Self::new_with_version(
            Self::VERSION,
            operation,
            package,
            label,
            changes,
            semantic_changes,
            semantic_selection,
            None,
            Vec::new(),
            Vec::new(),
        )
    }

    fn new_with_version(
        encoding_version: u8,
        operation: BuiltinIntentOperation,
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        mut changes: Vec<BuiltinSourceChange>,
        mut semantic_changes: Vec<BuiltinSemanticChange>,
        semantic_selection: Option<BuiltinSemanticSelectionIntent>,
        operation_key: Option<[u8; 32]>,
        mut capture_changes: Vec<BuiltinCaptureChange>,
        mut source_facts_changes: Vec<BuiltinSourceFactsChange>,
    ) -> Result<Self, BuiltinModelError> {
        if !matches!(
            encoding_version,
            3 | Self::VERSION
                | Self::KEYED_VERSION
                | Self::CAPTURE_VERSION
                | Self::FACTS_VERSION
                | Self::TYPED_FAILURE_VERSION
        ) || (encoding_version < Self::VERSION
            && (matches!(operation, BuiltinIntentOperation::SelectSemanticGeneration)
                || semantic_selection.is_some()))
            || (encoding_version == Self::KEYED_VERSION
                && operation_key.is_none_or(|key| key.iter().all(|byte| *byte == 0)))
            || (encoding_version != Self::KEYED_VERSION
                && encoding_version != Self::CAPTURE_VERSION
                && encoding_version != Self::FACTS_VERSION
                && encoding_version != Self::TYPED_FAILURE_VERSION
                && operation_key.is_some())
        {
            return Err(BuiltinModelError(
                "unsupported builtin intent encoding version".to_owned(),
            ));
        }
        let label = label.into();
        if label.len() > Self::MAX_LABEL_BYTES {
            return Err(BuiltinModelError(
                "package intent label is too large".to_owned(),
            ));
        }
        if backend_engine::PackageKey::from_value(label.as_str()) != package {
            return Err(BuiltinModelError(
                "package intent key does not match its canonical coordinate".to_owned(),
            ));
        }
        changes.sort_by_key(|change| change.key);
        let maximum_source_changes = BuiltinPackageRecord::MAX_PROJECT_FILES
            .saturating_mul(2)
            .saturating_add(BuiltinPackageRecord::MAX_PROJECT_MEMBERSHIP_PAGES.saturating_mul(2))
            .saturating_add(1);
        if changes.len() > maximum_source_changes
            || changes
                .windows(2)
                .any(|window| window[0].key >= window[1].key)
        {
            return Err(BuiltinModelError(
                "product source intent is empty, unordered, or oversized".to_owned(),
            ));
        }
        semantic_changes.sort_by(|left, right| left.key.cmp(&right.key));
        for change in &semantic_changes {
            if change.key.package_key() != package {
                return Err(BuiltinModelError(
                    "semantic publication intent crosses its package boundary".to_owned(),
                ));
            }
            if let Some(record) = &change.after {
                change.key.admit_record(record).map_err(|error| {
                    BuiltinModelError(format!("semantic publication intent: {error}"))
                })?;
            }
        }
        capture_changes.sort_by(|left, right| left.key.cmp(&right.key));
        if capture_changes.len() > backend_engine::application::MAX_MANIFEST_ENTRIES
            || (encoding_version == Self::CAPTURE_VERSION && capture_changes.is_empty())
            || capture_changes
                .windows(2)
                .any(|window| window[0].key >= window[1].key)
            || capture_changes.iter().any(|change| {
                !change.key.is_selected()
                    || change.key.package_key() != package
                    || (change.compiler_failure.is_some()
                        && (encoding_version != Self::TYPED_FAILURE_VERSION
                            || !matches!(
                                change.outcome,
                                ProductSemanticCaptureOutcome::Unavailable { .. }
                                    | ProductSemanticCaptureOutcome::Failed { .. }
                            )))
                    || change.capture.operation_key().is_some_and(|key| {
                        key.iter().all(|byte| *byte == 0)
                            || operation_key.is_some_and(|bound| &bound != key)
                    })
            })
        {
            return Err(BuiltinModelError(
                "semantic capture intent is unordered, oversized, or crosses its package boundary"
                    .to_owned(),
            ));
        }
        source_facts_changes.sort_by_key(|change| change.key);
        if source_facts_changes.len() > BuiltinPackageRecord::MAX_PROJECT_FILES.saturating_mul(300)
            || (encoding_version == Self::FACTS_VERSION && source_facts_changes.is_empty())
            || (encoding_version < Self::FACTS_VERSION && !source_facts_changes.is_empty())
            || source_facts_changes
                .windows(2)
                .any(|window| window[0].key >= window[1].key)
            || source_facts_changes.iter().any(|change| {
                change.expected == change.after
                    || [change.expected.as_ref(), change.after.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|record| {
                            let Some(owner) = record.file_key() else {
                                return true;
                            };
                            product_source_file_facts_record_key(owner, record)
                                .map_or(true, |canonical| canonical != change.key)
                        })
            })
        {
            return Err(BuiltinModelError(
                "source-facts intent is unordered, oversized, or contains an invalid owner"
                    .to_owned(),
            ));
        }
        if (changes.is_empty()
            && semantic_changes.is_empty()
            && capture_changes.is_empty()
            && source_facts_changes.is_empty())
            || semantic_changes.len() > backend_engine::application::MAX_MANIFEST_ENTRIES
            || semantic_changes
                .windows(2)
                .any(|window| window[0].key >= window[1].key)
        {
            return Err(BuiltinModelError(
                "semantic publication intent is empty, unordered, or oversized".to_owned(),
            ));
        }
        match (operation, semantic_selection.as_ref()) {
            (BuiltinIntentOperation::SelectSemanticGeneration, Some(selection)) => {
                selection.admit(package, &changes, &semantic_changes)?
            }
            (BuiltinIntentOperation::SelectSemanticGeneration, None) => {
                return Err(BuiltinModelError(
                    "semantic selection intent has no transition evidence".to_owned(),
                ));
            }
            (_, Some(_)) => {
                return Err(BuiltinModelError(
                    "non-selection intent carries semantic selection evidence".to_owned(),
                ));
            }
            (_, None) => {}
        }
        Ok(Self {
            encoding_version,
            operation,
            package,
            label,
            changes: changes.into_boxed_slice(),
            semantic_changes: semantic_changes.into_boxed_slice(),
            capture_changes: capture_changes.into_boxed_slice(),
            source_facts_changes: source_facts_changes.into_boxed_slice(),
            semantic_selection,
            operation_key,
        })
    }

    /// Binds the caller-owned durable operation key into the exact committed
    /// intent identity. Legacy unkeyed intents keep their existing encoding.
    pub(super) fn with_operation_key(
        mut self,
        operation_key: backend_library::IndexOperationKey,
    ) -> Result<Self, BuiltinModelError> {
        if self.encoding_version < Self::CAPTURE_VERSION {
            self.encoding_version = Self::KEYED_VERSION;
        }
        self.operation_key = Some(operation_key.to_bytes());
        if self
            .operation_key
            .is_none_or(|key| key.iter().all(|byte| *byte == 0))
            || self.capture_changes.iter().any(|change| {
                change
                    .capture
                    .operation_key()
                    .is_some_and(|key| key != &operation_key.to_bytes())
            })
        {
            return Err(BuiltinModelError(
                "keyed builtin intent has invalid operation identity".to_owned(),
            ));
        }
        Ok(self)
    }

    pub(super) fn with_capture_changes(
        self,
        capture_changes: Vec<BuiltinCaptureChange>,
    ) -> Result<Self, BuiltinModelError> {
        let minimum_version = if capture_changes
            .iter()
            .any(|change| change.compiler_failure.is_some())
        {
            Self::TYPED_FAILURE_VERSION
        } else {
            Self::CAPTURE_VERSION
        };
        Self::new_with_version(
            self.encoding_version.max(minimum_version),
            self.operation,
            self.package,
            self.label,
            self.changes.into_vec(),
            self.semantic_changes.into_vec(),
            self.semantic_selection,
            self.operation_key,
            capture_changes,
            self.source_facts_changes.into_vec(),
        )
    }

    pub(super) fn encode(&self) -> Vec<u8> {
        self.encode_canonical()
    }

    fn encode_canonical(&self) -> Vec<u8> {
        let label = self.label.as_bytes();
        let mut bytes = Vec::with_capacity(46 + label.len());
        bytes.extend_from_slice(match self.encoding_version {
            3 => b"BPI3",
            4 => b"BPI4",
            5 => b"BPI5",
            6 => b"BPI6",
            7 => b"BPI7",
            _ => b"BPI8",
        });
        bytes.push(self.encoding_version);
        bytes.push(match self.operation {
            BuiltinIntentOperation::Add => Self::ADD,
            BuiltinIntentOperation::Remove => Self::REMOVE,
            BuiltinIntentOperation::Index => Self::INDEX,
            BuiltinIntentOperation::SelectSemanticGeneration => Self::SELECT_SEMANTIC_GENERATION,
        });
        bytes.extend_from_slice(self.package.as_bytes());
        let label_len = u32::try_from(label.len()).unwrap_or(u32::MAX);
        bytes.extend_from_slice(&label_len.to_be_bytes());
        bytes.extend_from_slice(label);
        let change_count = u32::try_from(self.changes.len()).unwrap_or(u32::MAX);
        bytes.extend_from_slice(&change_count.to_be_bytes());
        for change in &self.changes {
            bytes.extend_from_slice(&change.key);
            match &change.after {
                Some(record) => {
                    bytes.push(1);
                    let mut encoded = Vec::new();
                    <ProductRelation as Relation>::encode_value(record, &mut encoded);
                    let length = u32::try_from(encoded.len()).unwrap_or(u32::MAX);
                    bytes.extend_from_slice(&length.to_be_bytes());
                    bytes.extend_from_slice(&encoded);
                }
                None => bytes.push(0),
            }
        }
        let semantic_count = u32::try_from(self.semantic_changes.len()).unwrap_or(u32::MAX);
        bytes.extend_from_slice(&semantic_count.to_be_bytes());
        for change in &self.semantic_changes {
            let mut key = Vec::new();
            ProductSemanticPublicationRelation::encode_key(&change.key, &mut key);
            bytes.extend_from_slice(&u32::try_from(key.len()).unwrap_or(u32::MAX).to_be_bytes());
            bytes.extend_from_slice(&key);
            match &change.after {
                Some(record) => {
                    bytes.push(1);
                    let mut value = Vec::new();
                    ProductSemanticPublicationRelation::encode_value(record, &mut value);
                    bytes.extend_from_slice(
                        &u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes(),
                    );
                    bytes.extend_from_slice(&value);
                }
                None => bytes.push(0),
            }
        }
        if self.encoding_version >= Self::VERSION {
            match &self.semantic_selection {
                None => bytes.push(0),
                Some(selection) => {
                    bytes.push(1);
                    encode_semantic_key(&selection.selected, &mut bytes);
                    encode_semantic_key(&selection.generation, &mut bytes);
                    encode_optional_semantic_record(selection.before.as_ref(), &mut bytes);
                    encode_semantic_record(&selection.after, &mut bytes);
                }
            }
        }
        if self.encoding_version == Self::KEYED_VERSION {
            bytes.push(1);
            bytes.extend_from_slice(
                &self
                    .operation_key
                    .expect("keyed intents are validated at construction"),
            );
        } else if self.encoding_version >= Self::CAPTURE_VERSION {
            match self.operation_key {
                Some(key) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&key);
                }
                None => bytes.push(0),
            }
            bytes.extend_from_slice(
                &u32::try_from(self.capture_changes.len())
                    .unwrap_or(u32::MAX)
                    .to_be_bytes(),
            );
            for change in &self.capture_changes {
                encode_semantic_key(&change.key, &mut bytes);
                encode_optional_capture_record(change.expected.as_ref(), &mut bytes);
                encode_source_capture(change.capture, &mut bytes);
                encode_capture_outcome(change.outcome, &mut bytes);
                if self.encoding_version >= Self::TYPED_FAILURE_VERSION {
                    encode_optional_compiler_failure(change.compiler_failure.as_ref(), &mut bytes);
                }
            }
            if self.encoding_version >= Self::FACTS_VERSION {
                bytes.extend_from_slice(
                    &u32::try_from(self.source_facts_changes.len())
                        .unwrap_or(u32::MAX)
                        .to_be_bytes(),
                );
                for change in &self.source_facts_changes {
                    bytes.extend_from_slice(&change.key);
                    encode_optional_source_facts_record(change.expected.as_ref(), &mut bytes);
                    encode_optional_source_facts_record(change.after.as_ref(), &mut bytes);
                }
            }
        }
        bytes
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        self.encode_canonical()
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, BuiltinModelError> {
        let mut decoder = IntentDecoder::open(bytes)?;
        let encoding_version = decoder.version;
        let operation = decoder.operation()?;
        let (package, label) = decoder.package()?;
        let changes = decoder.source_changes()?;
        let semantic_changes = decoder.semantic_changes()?;
        let semantic_selection = decoder.semantic_selection()?;
        let operation_key = decoder.operation_key()?;
        let capture_changes = decoder.capture_changes()?;
        let source_facts_changes = decoder.source_facts_changes()?;
        decoder.finish()?;
        let mut intent = Self::new_with_version(
            encoding_version,
            operation,
            package,
            label,
            changes,
            semantic_changes,
            semantic_selection,
            operation_key,
            capture_changes,
            source_facts_changes,
        )?;
        if encoding_version < Self::CAPTURE_VERSION && !intent.capture_changes.is_empty() {
            return Err(BuiltinModelError(
                "capture markers require the v6 intent encoding".to_owned(),
            ));
        }
        Ok(intent)
    }

    pub(super) fn record(&self) -> Result<BuiltinPackageRecord, BuiltinModelError> {
        self.changes
            .iter()
            .find(|change| change.key == self.package.to_bytes())
            .and_then(|change| change.after.clone())
            .ok_or_else(|| BuiltinModelError("intent has no project record".to_owned()))
    }

    pub(super) fn changes(&self) -> &[BuiltinSourceChange] {
        &self.changes
    }

    pub(super) fn semantic_changes(&self) -> &[BuiltinSemanticChange] {
        &self.semantic_changes
    }

    pub(super) fn capture_changes(&self) -> &[BuiltinCaptureChange] {
        &self.capture_changes
    }

    pub(super) fn source_facts_changes(&self) -> &[BuiltinSourceFactsChange] {
        &self.source_facts_changes
    }

    fn admit_semantic_selection_against(
        &self,
        relation: &WorkspaceRelationHandle<BuiltinSemanticRelation>,
    ) -> Result<(), BuiltinModelError> {
        let Some(selection) = &self.semantic_selection else {
            return Ok(());
        };
        let selected = relation
            .lookup(&selection.selected)
            .map_err(|error| BuiltinModelError(format!("read semantic selection base: {error}")))?;
        if selected != selection.before {
            return Err(BuiltinModelError(
                "semantic selection base does not match its persisted before value".to_owned(),
            ));
        }
        let generation = relation.lookup(&selection.generation).map_err(|error| {
            BuiltinModelError(format!("read semantic selection history: {error}"))
        })?;
        if generation.as_ref() != Some(&selection.after) {
            return Err(BuiltinModelError(
                "semantic selection generation is absent or changed".to_owned(),
            ));
        }
        Ok(())
    }
}

impl BuiltinSemanticSelectionIntent {
    fn admit(
        &self,
        package: backend_engine::PackageKey,
        source_changes: &[BuiltinSourceChange],
        semantic_changes: &[BuiltinSemanticChange],
    ) -> Result<(), BuiltinModelError> {
        if !self.selected.is_selected() || self.selected.package_key() != package {
            return Err(BuiltinModelError(
                "semantic selection target does not match its package intent".to_owned(),
            ));
        }
        let backend_engine::SemanticPublicationSelection::Generation(identity) =
            self.generation.selection()
        else {
            return Err(BuiltinModelError(
                "semantic selection has no immutable generation key".to_owned(),
            ));
        };
        if self.selected.for_generation(identity) != self.generation {
            return Err(BuiltinModelError(
                "semantic selection generation crosses its target".to_owned(),
            ));
        }
        self.generation.admit_record(&self.after).map_err(|error| {
            BuiltinModelError(format!("semantic selection generation: {error}"))
        })?;
        if self.before.as_ref() == Some(&self.after)
            || !source_changes.is_empty()
            || semantic_changes
                != [BuiltinSemanticChange {
                    key: self.selected.clone(),
                    after: Some(self.after.clone()),
                }]
        {
            return Err(BuiltinModelError(
                "semantic selection does not encode one exact before-to-after change".to_owned(),
            ));
        }
        Ok(())
    }
}

fn encode_semantic_key(key: &ProductSemanticPublicationKey, output: &mut Vec<u8>) {
    let mut encoded = Vec::new();
    ProductSemanticPublicationRelation::encode_key(key, &mut encoded);
    output.extend_from_slice(
        &u32::try_from(encoded.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    output.extend_from_slice(&encoded);
}

fn encode_semantic_record(record: &ProductSemanticPublicationRecord, output: &mut Vec<u8>) {
    let mut encoded = Vec::new();
    ProductSemanticPublicationRelation::encode_value(record, &mut encoded);
    output.extend_from_slice(
        &u32::try_from(encoded.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    output.extend_from_slice(&encoded);
}

fn encode_optional_semantic_record(
    record: Option<&ProductSemanticPublicationRecord>,
    output: &mut Vec<u8>,
) {
    match record {
        None => output.push(0),
        Some(record) => {
            output.push(1);
            encode_semantic_record(record, output);
        }
    }
}

fn encode_optional_capture_record(
    record: Option<&ProductSemanticCaptureRecord>,
    output: &mut Vec<u8>,
) {
    match record {
        None => output.push(0),
        Some(record) => {
            output.push(1);
            let mut encoded = Vec::new();
            ProductSemanticCaptureRelation::encode_value(record, &mut encoded);
            output.extend_from_slice(
                &u32::try_from(encoded.len())
                    .unwrap_or(u32::MAX)
                    .to_be_bytes(),
            );
            output.extend_from_slice(&encoded);
        }
    }
}

fn encode_optional_source_facts_record(
    record: Option<&ProductSourceFileFactsRecord>,
    output: &mut Vec<u8>,
) {
    match record {
        None => output.push(0),
        Some(record) => {
            output.push(1);
            let mut encoded = Vec::new();
            ProductSourceFileFactsRelation::encode_value(record, &mut encoded);
            output.extend_from_slice(
                &u32::try_from(encoded.len())
                    .unwrap_or(u32::MAX)
                    .to_be_bytes(),
            );
            output.extend_from_slice(&encoded);
        }
    }
}

fn encode_source_capture(capture: SemanticSourceCapture, output: &mut Vec<u8>) {
    match capture.operation_key() {
        None => output.push(0),
        Some(key) => {
            output.push(1);
            output.extend_from_slice(key);
        }
    }
    output.extend_from_slice(capture.source_version());
    output.extend_from_slice(capture.input_digest());
    output.extend_from_slice(&capture.observation_sequence().to_be_bytes());
    output.extend_from_slice(&capture.source_count().to_be_bytes());
}

fn encode_capture_outcome(outcome: ProductSemanticCaptureOutcome, output: &mut Vec<u8>) {
    match outcome {
        ProductSemanticCaptureOutcome::Pending { prior } => {
            output.push(1);
            encode_optional_prior(prior, output);
        }
        ProductSemanticCaptureOutcome::Unavailable { reason } => {
            output.push(2);
            output.push(reason as u8);
        }
        ProductSemanticCaptureOutcome::Failed { prior, reason } => {
            output.push(3);
            encode_prior(prior, output);
            output.push(reason as u8);
        }
        ProductSemanticCaptureOutcome::Published { coverage, claim } => {
            output.push(4);
            encode_semantic_record(
                &ProductSemanticPublicationRecord::Published { coverage, claim },
                output,
            );
        }
    }
}

fn encode_optional_compiler_failure(
    failure: Option<&backend_library::PackageCompilerFailure>,
    output: &mut Vec<u8>,
) {
    match failure {
        None => output.push(0),
        Some(failure) => {
            if let Ok(bytes) = failure.encode_bounded_json() {
                output.push(1);
                output.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                output.extend_from_slice(&bytes);
            } else {
                // Construction admits only bounded, serde-safe closed DTOs.
                // This sentinel is rejected by the decoder if that invariant
                // is ever broken, rather than encoding a partial refusal.
                output.push(u8::MAX);
            }
        }
    }
}

fn encode_optional_prior(prior: Option<SemanticPublicationVersion>, output: &mut Vec<u8>) {
    match prior {
        None => output.push(0),
        Some(prior) => {
            output.push(1);
            encode_prior(prior, output);
        }
    }
}

fn encode_prior(prior: SemanticPublicationVersion, output: &mut Vec<u8>) {
    encode_semantic_record(
        &ProductSemanticPublicationRecord::Published {
            coverage: prior.coverage(),
            claim: prior.claim(),
        },
        output,
    );
}

/// A version-pinned package URL already selects a compiler family. Adding it
/// without a checked source/project authority records that exact semantic
/// terminal instead of manufacturing declarations from package metadata.
/// Local labels and generic C/C++ coordinates remain without a row until an
/// index request supplies an explicit language profile.
fn added_package_semantic_terminal(
    package: backend_engine::PackageKey,
    label: &str,
) -> Result<Vec<BuiltinSemanticChange>, BuiltinModelError> {
    if !label.starts_with("pkg:") {
        return Ok(Vec::new());
    }
    let coordinate = backend_semantic::vocabulary::PackageUrl::parse(label.to_owned())
        .map_err(|error| BuiltinModelError(format!("added package URL: {error:?}")))?;
    let profile = match coordinate.package_type() {
        backend_semantic::vocabulary::PackageType::Cargo => {
            backend_semantic::vocabulary::LanguageProfile::Rust(
                backend_semantic::vocabulary::RustEdition::Rust2024,
            )
        }
        backend_semantic::vocabulary::PackageType::Npm => {
            backend_semantic::vocabulary::LanguageProfile::TypeScript(
                backend_semantic::vocabulary::TypeScriptSource::TypeScript,
            )
        }
        backend_semantic::vocabulary::PackageType::Pypi => {
            backend_semantic::vocabulary::LanguageProfile::Python(
                backend_semantic::vocabulary::PythonVersion::Python314,
            )
        }
        backend_semantic::vocabulary::PackageType::Golang => {
            backend_semantic::vocabulary::LanguageProfile::Go(
                backend_semantic::vocabulary::GoVersion::Go125,
            )
        }
        backend_semantic::vocabulary::PackageType::Maven => {
            backend_semantic::vocabulary::LanguageProfile::Java(
                backend_semantic::vocabulary::JavaRelease::Java25,
            )
        }
        backend_semantic::vocabulary::PackageType::Nuget => {
            backend_semantic::vocabulary::LanguageProfile::CSharp(
                backend_semantic::vocabulary::CSharpVersion::CSharp14,
            )
        }
        // The `generic` package type intentionally cannot choose between C and
        // C++; indexing a concrete extension supplies that distinction.
        backend_semantic::vocabulary::PackageType::Generic => return Ok(Vec::new()),
    };
    let package_reference = backend_engine::PackageReference::parse(label.to_owned())
        .map_err(|error| BuiltinModelError(format!("added package reference: {error:?}")))?;
    if backend_engine::package_key(package_reference.as_str()) != package {
        return Err(BuiltinModelError(
            "added semantic package reference does not match its product key".to_owned(),
        ));
    }
    let key = ProductSemanticPublicationKey::new(package_reference, coordinate, profile)
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
    Ok(vec![BuiltinSemanticChange {
        key,
        after: Some(ProductSemanticPublicationRecord::Unavailable(
            backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority,
        )),
    }])
}

impl backend_engine::queue::QueueSized for BuiltinIntent {
    fn queue_bytes(&self) -> usize {
        self.encode().len()
    }
}

struct IntentDecoder<'a> {
    bytes: &'a [u8],
    at: usize,
    version: u8,
}

impl<'a> IntentDecoder<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, BuiltinModelError> {
        let version = bytes
            .get(4)
            .copied()
            .ok_or_else(|| BuiltinModelError("malformed builtin package intent".to_owned()))?;
        if bytes.len() < 50
            || !matches!(
                (bytes.get(..4), version),
                (Some(b"BPI3"), 3)
                    | (Some(b"BPI4"), 4)
                    | (Some(b"BPI5"), 5)
                    | (Some(b"BPI6"), 6)
                    | (Some(b"BPI7"), 7)
                    | (Some(b"BPI8"), 8)
            )
        {
            return Err(BuiltinModelError(
                "malformed builtin package intent".to_owned(),
            ));
        }
        Ok(Self {
            bytes,
            at: 5,
            version,
        })
    }

    fn operation(&mut self) -> Result<BuiltinIntentOperation, BuiltinModelError> {
        match self.take(1)?.first().copied() {
            Some(BuiltinIntent::ADD) => Ok(BuiltinIntentOperation::Add),
            Some(BuiltinIntent::REMOVE) => Ok(BuiltinIntentOperation::Remove),
            Some(BuiltinIntent::INDEX) => Ok(BuiltinIntentOperation::Index),
            Some(BuiltinIntent::SELECT_SEMANTIC_GENERATION) if self.version >= 4 => {
                Ok(BuiltinIntentOperation::SelectSemanticGeneration)
            }
            _ => Err(BuiltinModelError(
                "unknown builtin package operation".to_owned(),
            )),
        }
    }

    fn package(&mut self) -> Result<(backend_engine::PackageKey, String), BuiltinModelError> {
        let encoded: [u8; 32] = self
            .take(32)?
            .try_into()
            .map_err(|_| BuiltinModelError("malformed package intent key".to_owned()))?;
        let label_len = self.read_u32()? as usize;
        if label_len > BuiltinIntent::MAX_LABEL_BYTES {
            return Err(BuiltinModelError(
                "malformed package intent label".to_owned(),
            ));
        }
        let label = String::from_utf8(self.take(label_len)?.to_vec())
            .map_err(|_| BuiltinModelError("package intent label is not utf8".to_owned()))?;
        let package = backend_engine::PackageKey::from_value(label.as_str());
        if package.to_bytes() != encoded {
            return Err(BuiltinModelError(
                "package intent key does not match its canonical coordinate".to_owned(),
            ));
        }
        Ok((package, label))
    }

    fn source_changes(&mut self) -> Result<Vec<BuiltinSourceChange>, BuiltinModelError> {
        let count = self.read_u32()? as usize;
        let maximum_source_changes = BuiltinPackageRecord::MAX_PROJECT_FILES
            .saturating_mul(2)
            .saturating_add(BuiltinPackageRecord::MAX_PROJECT_MEMBERSHIP_PAGES.saturating_mul(2))
            .saturating_add(1);
        if count > maximum_source_changes {
            return Err(BuiltinModelError(
                "malformed product source change count".to_owned(),
            ));
        }
        (0..count).map(|_| self.source_change()).collect()
    }

    fn source_change(&mut self) -> Result<BuiltinSourceChange, BuiltinModelError> {
        let key = self
            .take(32)?
            .try_into()
            .map_err(|_| BuiltinModelError("malformed product source key".to_owned()))?;
        let after = match self.take(1)?.first().copied() {
            Some(0) => None,
            Some(1) => {
                let length = self.read_u32()? as usize;
                Some(
                    <ProductRelation as CanonicalRelation>::decode_value(self.take(length)?)
                        .map_err(|_| {
                            BuiltinModelError("malformed product source record".to_owned())
                        })?,
                )
            }
            _ => {
                return Err(BuiltinModelError(
                    "malformed product source operation".to_owned(),
                ));
            }
        };
        Ok(BuiltinSourceChange { key, after })
    }

    fn semantic_changes(&mut self) -> Result<Vec<BuiltinSemanticChange>, BuiltinModelError> {
        let count = self.read_u32()? as usize;
        if count > backend_engine::application::MAX_MANIFEST_ENTRIES {
            return Err(BuiltinModelError(
                "malformed semantic publication change count".to_owned(),
            ));
        }
        (0..count).map(|_| self.semantic_change()).collect()
    }

    fn semantic_change(&mut self) -> Result<BuiltinSemanticChange, BuiltinModelError> {
        let key_length = self.read_u32()? as usize;
        let key = ProductSemanticPublicationRelation::decode_key(self.take(key_length)?)
            .map_err(|_| BuiltinModelError("malformed semantic publication key".to_owned()))?;
        let after = match self.take(1)?.first().copied() {
            Some(0) => None,
            Some(1) => {
                let length = self.read_u32()? as usize;
                Some(
                    ProductSemanticPublicationRelation::decode_value(self.take(length)?).map_err(
                        |_| BuiltinModelError("malformed semantic publication record".to_owned()),
                    )?,
                )
            }
            _ => {
                return Err(BuiltinModelError(
                    "malformed semantic publication operation".to_owned(),
                ));
            }
        };
        Ok(BuiltinSemanticChange { key, after })
    }

    fn semantic_selection(
        &mut self,
    ) -> Result<Option<BuiltinSemanticSelectionIntent>, BuiltinModelError> {
        if self.version < 4 {
            return Ok(None);
        }
        match self.take(1)?.first().copied() {
            Some(0) => Ok(None),
            Some(1) => {
                let selected = self.semantic_key()?;
                let generation = self.semantic_key()?;
                let before = self.optional_semantic_record()?;
                let after = self.semantic_record()?;
                Ok(Some(BuiltinSemanticSelectionIntent {
                    selected,
                    generation,
                    before,
                    after,
                }))
            }
            _ => Err(BuiltinModelError(
                "malformed semantic selection evidence tag".to_owned(),
            )),
        }
    }

    fn operation_key(&mut self) -> Result<Option<[u8; 32]>, BuiltinModelError> {
        if self.version < BuiltinIntent::KEYED_VERSION {
            return Ok(None);
        }
        match self.take(1)?.first().copied() {
            Some(1) => {
                let key: [u8; 32] = self
                    .take(32)?
                    .try_into()
                    .map_err(|_| BuiltinModelError("malformed index operation key".to_owned()))?;
                if key.iter().all(|byte| *byte == 0) {
                    return Err(BuiltinModelError(
                        "malformed index operation key".to_owned(),
                    ));
                }
                Ok(Some(key))
            }
            Some(0) if self.version >= BuiltinIntent::CAPTURE_VERSION => Ok(None),
            _ => Err(BuiltinModelError(
                "malformed index operation key marker".to_owned(),
            )),
        }
    }

    fn capture_changes(&mut self) -> Result<Vec<BuiltinCaptureChange>, BuiltinModelError> {
        if self.version < BuiltinIntent::CAPTURE_VERSION {
            return Ok(Vec::new());
        }
        let count = self.read_u32()? as usize;
        if (count == 0 && self.version == BuiltinIntent::CAPTURE_VERSION)
            || count > backend_engine::application::MAX_MANIFEST_ENTRIES
        {
            return Err(BuiltinModelError(
                "malformed semantic capture change count".to_owned(),
            ));
        }
        (0..count).map(|_| self.capture_change()).collect()
    }

    fn source_facts_changes(&mut self) -> Result<Vec<BuiltinSourceFactsChange>, BuiltinModelError> {
        if self.version < BuiltinIntent::FACTS_VERSION {
            return Ok(Vec::new());
        }
        let count = self.read_u32()? as usize;
        let maximum = BuiltinPackageRecord::MAX_PROJECT_FILES.saturating_mul(300);
        let minimum_bytes_per_change = 34usize;
        if (count == 0 && self.version == BuiltinIntent::FACTS_VERSION)
            || count > maximum
            || count > self.bytes.len().saturating_sub(self.at) / minimum_bytes_per_change
        {
            return Err(BuiltinModelError(
                "malformed source-facts change count".to_owned(),
            ));
        }
        (0..count).map(|_| self.source_facts_change()).collect()
    }

    fn source_facts_change(&mut self) -> Result<BuiltinSourceFactsChange, BuiltinModelError> {
        let key = self
            .take(32)?
            .try_into()
            .map_err(|_| BuiltinModelError("malformed source-facts key".to_owned()))?;
        let expected = self.optional_source_facts_record()?;
        let after = self.optional_source_facts_record()?;
        Ok(BuiltinSourceFactsChange {
            key,
            expected,
            after,
        })
    }

    fn optional_source_facts_record(
        &mut self,
    ) -> Result<Option<ProductSourceFileFactsRecord>, BuiltinModelError> {
        match self.take(1)?.first().copied() {
            Some(0) => Ok(None),
            Some(1) => {
                let length = self.read_u32()? as usize;
                ProductSourceFileFactsRelation::decode_value(self.take(length)?)
                    .map(Some)
                    .map_err(|_| BuiltinModelError("malformed source-facts record".to_owned()))
            }
            _ => Err(BuiltinModelError(
                "malformed source-facts record tag".to_owned(),
            )),
        }
    }

    fn capture_change(&mut self) -> Result<BuiltinCaptureChange, BuiltinModelError> {
        let key = self.semantic_key()?;
        let expected = match self.take(1)?.first().copied() {
            Some(0) => None,
            Some(1) => {
                let length = self.read_u32()? as usize;
                Some(
                    ProductSemanticCaptureRelation::decode_value(self.take(length)?).map_err(
                        |_| BuiltinModelError("malformed semantic capture record".to_owned()),
                    )?,
                )
            }
            _ => {
                return Err(BuiltinModelError(
                    "malformed semantic capture before tag".to_owned(),
                ));
            }
        };
        let capture = self.source_capture()?;
        let outcome = self.capture_outcome()?;
        let compiler_failure = if self.version >= BuiltinIntent::TYPED_FAILURE_VERSION {
            self.optional_compiler_failure()?
        } else {
            None
        };
        Ok(BuiltinCaptureChange {
            key,
            expected,
            capture,
            outcome,
            compiler_failure,
        })
    }

    fn optional_compiler_failure(
        &mut self,
    ) -> Result<Option<backend_library::PackageCompilerFailure>, BuiltinModelError> {
        match self.take(1)?.first().copied() {
            Some(0) => Ok(None),
            Some(1) => {
                let length = self.read_u32()? as usize;
                if length == 0
                    || length > backend_library::PackageCompilerFailure::MAX_ENCODED_BYTES
                {
                    return Err(BuiltinModelError(
                        "malformed typed compiler-failure length".to_owned(),
                    ));
                }
                let bytes = self.take(length)?;
                let failure = backend_library::PackageCompilerFailure::decode_bounded_json(bytes)
                    .map_err(|_| {
                    BuiltinModelError("malformed typed compiler-failure payload".to_owned())
                })?;
                let canonical = failure.encode_bounded_json().map_err(|_| {
                    BuiltinModelError("could not re-encode typed compiler failure".to_owned())
                })?;
                if canonical.as_slice() != bytes {
                    return Err(BuiltinModelError(
                        "noncanonical typed compiler-failure payload".to_owned(),
                    ));
                }
                Ok(Some(failure))
            }
            _ => Err(BuiltinModelError(
                "malformed typed compiler-failure tag".to_owned(),
            )),
        }
    }

    fn source_capture(&mut self) -> Result<SemanticSourceCapture, BuiltinModelError> {
        let operation_key =
            match self.take(1)?.first().copied() {
                Some(0) => None,
                Some(1) => Some(self.take(32)?.try_into().map_err(|_| {
                    BuiltinModelError("malformed semantic operation key".to_owned())
                })?),
                _ => {
                    return Err(BuiltinModelError(
                        "malformed semantic operation key tag".to_owned(),
                    ));
                }
            };
        let source_version = self
            .take(32)?
            .try_into()
            .map_err(|_| BuiltinModelError("malformed semantic source version".to_owned()))?;
        let input_digest = self
            .take(32)?
            .try_into()
            .map_err(|_| BuiltinModelError("malformed semantic input digest".to_owned()))?;
        let observation_sequence = self.read_u64()?;
        let source_count = self.read_u64()?;
        SemanticSourceCapture::new(
            operation_key,
            source_version,
            input_digest,
            observation_sequence,
            source_count,
        )
        .map_err(|error| BuiltinModelError(error.to_owned()))
    }

    fn capture_outcome(&mut self) -> Result<ProductSemanticCaptureOutcome, BuiltinModelError> {
        match self.take(1)?.first().copied() {
            Some(1) => Ok(ProductSemanticCaptureOutcome::Pending {
                prior: self.optional_prior()?,
            }),
            Some(2) => Ok(ProductSemanticCaptureOutcome::Unavailable {
                reason: self.unavailable_reason()?,
            }),
            Some(3) => Ok(ProductSemanticCaptureOutcome::Failed {
                prior: self.prior()?,
                reason: self.unavailable_reason()?,
            }),
            Some(4) => match self.semantic_record()? {
                ProductSemanticPublicationRecord::Published { coverage, claim } => {
                    Ok(ProductSemanticCaptureOutcome::Published { coverage, claim })
                }
                ProductSemanticPublicationRecord::Unavailable(_) => Err(BuiltinModelError(
                    "malformed published semantic capture outcome".to_owned(),
                )),
            },
            _ => Err(BuiltinModelError(
                "malformed semantic capture outcome".to_owned(),
            )),
        }
    }

    fn optional_prior(&mut self) -> Result<Option<SemanticPublicationVersion>, BuiltinModelError> {
        match self.take(1)?.first().copied() {
            Some(0) => Ok(None),
            Some(1) => self.prior().map(Some),
            _ => Err(BuiltinModelError("malformed semantic prior tag".to_owned())),
        }
    }

    fn prior(&mut self) -> Result<SemanticPublicationVersion, BuiltinModelError> {
        match self.semantic_record()? {
            ProductSemanticPublicationRecord::Published { coverage, claim } => {
                Ok(SemanticPublicationVersion::new(coverage, claim))
            }
            ProductSemanticPublicationRecord::Unavailable(_) => Err(BuiltinModelError(
                "semantic prior does not contain a published generation".to_owned(),
            )),
        }
    }

    fn unavailable_reason(
        &mut self,
    ) -> Result<backend_engine::builtin::SemanticUnavailableReason, BuiltinModelError> {
        match self.take(1)?.first().copied() {
            Some(1) => Ok(backend_engine::builtin::SemanticUnavailableReason::Toolchain),
            Some(2) => Ok(backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority),
            Some(3) => Ok(backend_engine::builtin::SemanticUnavailableReason::Cancelled),
            Some(4) => Ok(backend_engine::builtin::SemanticUnavailableReason::Rejected),
            _ => Err(BuiltinModelError(
                "malformed semantic unavailable reason".to_owned(),
            )),
        }
    }

    fn semantic_key(&mut self) -> Result<ProductSemanticPublicationKey, BuiltinModelError> {
        let length = self.read_u32()? as usize;
        ProductSemanticPublicationRelation::decode_key(self.take(length)?)
            .map_err(|_| BuiltinModelError("malformed semantic selection key".to_owned()))
    }

    fn optional_semantic_record(
        &mut self,
    ) -> Result<Option<ProductSemanticPublicationRecord>, BuiltinModelError> {
        match self.take(1)?.first().copied() {
            Some(0) => Ok(None),
            Some(1) => self.semantic_record().map(Some),
            _ => Err(BuiltinModelError(
                "malformed semantic selection before tag".to_owned(),
            )),
        }
    }

    fn semantic_record(&mut self) -> Result<ProductSemanticPublicationRecord, BuiltinModelError> {
        let length = self.read_u32()? as usize;
        ProductSemanticPublicationRelation::decode_value(self.take(length)?)
            .map_err(|_| BuiltinModelError("malformed semantic selection record".to_owned()))
    }

    fn read_u32(&mut self) -> Result<u32, BuiltinModelError> {
        let value = self
            .take(4)?
            .try_into()
            .map_err(|_| BuiltinModelError("malformed product source length".to_owned()))?;
        Ok(u32::from_be_bytes(value))
    }

    fn read_u64(&mut self) -> Result<u64, BuiltinModelError> {
        let value = self
            .take(8)?
            .try_into()
            .map_err(|_| BuiltinModelError("malformed semantic source count".to_owned()))?;
        Ok(u64::from_be_bytes(value))
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], BuiltinModelError> {
        let end = self
            .at
            .checked_add(length)
            .ok_or_else(|| BuiltinModelError("product source intent length overflow".to_owned()))?;
        let value = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| BuiltinModelError("truncated product source intent".to_owned()))?;
        self.at = end;
        Ok(value)
    }

    fn finish(self) -> Result<(), BuiltinModelError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(BuiltinModelError(
                "trailing product source intent bytes".to_owned(),
            ))
        }
    }
}

/// Failure while constructing or admitting the checked builtin workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinModelError(pub(super) String);

impl fmt::Display for BuiltinModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BuiltinModelError {}

/// The built in workspace model used by the local daemon.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinModel;

impl WorkspaceModel for BuiltinModel {
    type Intent = BuiltinIntent;
    type Error = BuiltinModelError;

    fn request_id(&self, intent: &Self::Intent) -> [u8; 32] {
        let bytes = intent.canonical_bytes();
        ObjectVersion::<BuiltinIntentSchema>::from_value(&bytes).to_bytes()
    }

    fn prepare(
        &self,
        base: &WorkspaceSnapshot,
        intent: &Self::Intent,
        transaction: TransactionId,
    ) -> Result<PreparedTransition, Self::Error> {
        let relation = base
            .relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
        let update = prepare_source_update(&relation, intent.package.to_bytes(), intent.changes())?;
        prepare_transition_with_source_update(base, intent, transaction, &relation, update)
    }

    fn admit_persisted(
        &self,
        _persisted: &backend_engine::PersistedTransition,
    ) -> Result<PreparedTransition, Self::Error> {
        Err(BuiltinModelError(
            "persisted builtin transitions require an owned durable relation loader".to_owned(),
        ))
    }

    fn admit_persisted_with_store(
        &self,
        persisted: &backend_engine::PersistedTransition,
        store: &backend_engine::FileStore,
    ) -> Result<PreparedTransition, Self::Error> {
        admit_persisted_transition(persisted, store)
            .map_err(|error| BuiltinModelError(format!("builtin persisted transition: {error}")))
    }
}

/// Builds the ordinary checked transition from an already admitted source
/// update. The historical fixture writer uses this same durable wire path.
pub(super) fn prepare_transition_with_source_update(
    base: &WorkspaceSnapshot,
    intent: &BuiltinIntent,
    transaction: TransactionId,
    relation: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    update: LazyPreparedUpdate<BuiltinWorkspaceRelation>,
) -> Result<PreparedTransition, BuiltinModelError> {
    let semantic = base
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
    intent.admit_semantic_selection_against(&semantic)?;
    let semantic_update = prepare_semantic_update(&semantic, intent.semantic_changes())?;
    let source_facts_update = prepare_source_facts_relation_update(base, intent)?;
    let source_change_count = update.delta().changes().count();
    let semantic_change_count = semantic_update.delta().changes().count();
    let source_changed = source_change_count != 0;
    let semantic_changed = semantic_change_count != 0;
    let changed_items =
        checked_product_change_count(source_change_count, semantic_change_count, intent)?;
    if changed_items == 0 {
        return Err(BuiltinModelError(
            "product source intent is a no-op".to_owned(),
        ));
    }
    let base_manifest =
        workspace_manifest_from_root(&relation.root_handle(), &semantic.root_handle())?;
    if base.manifest() != &base_manifest {
        return Err(BuiltinModelError(
            "workspace base manifest mismatch".to_owned(),
        ));
    }
    let target_manifest =
        workspace_manifest_from_root(&update.target_root(), &semantic_update.target_root())?;
    let target_root = update.target().root();
    let target_workspace_root = target_manifest.root();
    let semantic_target_root = semantic_update.target().root();
    let request = BuiltinModel.request_id(intent);
    let changed_nodes = update.changed_nodes().to_vec();
    let semantic_changed_nodes = semantic_update.changed_nodes().to_vec();
    let relation_base_object = relation
        .root_object()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let semantic_base_object = semantic
        .root_object()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let work =
        TransitionWork::from_lazy(changed_items, update.work()).map_err(BuiltinModelError)?;
    let relation_delta = update.into_delta();
    let semantic_delta = semantic_update.into_delta();
    let mut transitions = Vec::with_capacity(2);
    if source_changed {
        transitions.push(RelationTransition::from_delta(&relation_delta));
    }
    if semantic_changed {
        transitions.push(RelationTransition::from_delta(&semantic_delta));
    }
    let delta = WorkspaceDelta::new(&base_manifest, &target_manifest, transitions)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let provenance = CommitProvenance::from_versions(
        ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE),
        transaction.version(),
        request,
    );
    let commit = backend_engine::commit_checked(&target_manifest, Vec::new(), provenance)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let capture_update = prepare_capture_relation_update(
        base,
        intent,
        target_workspace_root,
        base.sequence(),
        request,
        *commit.id().as_bytes(),
        &semantic,
    )?;
    let closure = transition_closure_lazy(
        base.closure(),
        &target_manifest,
        LazyClosureUpdate {
            base_source: relation_base_object,
            base_semantic: semantic_base_object,
            changed_sources: &changed_nodes,
            source_root: target_root,
            changed_semantics: &semantic_changed_nodes,
            semantic_root: semantic_target_root,
            transaction,
            intent,
            capture_objects: &capture_update.node_objects,
            capture_pointer: capture_update.pointer.as_ref(),
            source_facts_objects: &source_facts_update.node_objects,
            source_facts_pointer: source_facts_update.pointer.as_ref(),
        },
    )?;
    let registry = super::product_relation_registry()?;
    let intent_bytes = intent.encode();
    let intent_key = ObjectKey::<BuiltinIntentSchema>::from_value(&intent_bytes);
    let intent_object = TypedObject::from_value(&intent_key, &intent_bytes);
    let transition = PreparedTransition::new_with_registry(
        request,
        transaction,
        delta,
        commit,
        closure,
        &registry,
    )
    .map_err(|error| BuiltinModelError(format!("construct prepared transition: {error}")))?;
    let transition = transition
        .retain_objects(capture_update.node_objects, &registry)
        .map_err(|error| BuiltinModelError(format!("retain semantic capture nodes: {error}")))?;
    let transition = if let Some(pointer) = capture_update.pointer {
        transition
            .replace_object_family([pointer], &registry)
            .map_err(|error| BuiltinModelError(format!("select semantic capture root: {error}")))?
    } else {
        transition
    };
    let transition = transition
        .retain_objects(source_facts_update.node_objects, &registry)
        .map_err(|error| BuiltinModelError(format!("retain source facts nodes: {error}")))?;
    let transition = if let Some(pointer) = source_facts_update.pointer {
        transition
            .replace_object_family([pointer], &registry)
            .map_err(|error| BuiltinModelError(format!("select source facts root: {error}")))?
    } else {
        transition
    };
    transition
        .replace_object_family([intent_object], &registry)
        .map(|transition| transition.with_work(work))
        .map_err(|error| BuiltinModelError(format!("retain current intent: {error}")))
}

struct PreparedCaptureRelationUpdate {
    node_objects: Vec<TypedObject>,
    pointer: Option<TypedObject>,
}

struct PreparedSourceFactsRelationUpdate {
    node_objects: Vec<TypedObject>,
    pointer: Option<TypedObject>,
}

fn checked_product_change_count(
    source_changes: usize,
    semantic_changes: usize,
    intent: &BuiltinIntent,
) -> Result<usize, BuiltinModelError> {
    [
        source_changes,
        semantic_changes,
        intent.source_facts_changes().len(),
        intent.capture_changes().len(),
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .ok_or_else(|| BuiltinModelError("product transition change count overflow".to_owned()))
}

fn prepare_source_facts_relation_update(
    base: &WorkspaceSnapshot,
    intent: &BuiltinIntent,
) -> Result<PreparedSourceFactsRelationUpdate, BuiltinModelError> {
    if intent.source_facts_changes().is_empty() {
        return Ok(PreparedSourceFactsRelationUpdate {
            node_objects: Vec::new(),
            pointer: None,
        });
    }
    let selected = product_source_file_facts_relation(base).map_err(|error| {
        BuiltinModelError(format!("open selected source facts relation: {error}"))
    })?;
    let changes = intent
        .source_facts_changes()
        .iter()
        .map(|change| TreeChange {
            key: change.key,
            after: change.after.clone(),
        })
        .collect::<Vec<_>>();
    if let Some(relation) = selected {
        for change in intent.source_facts_changes() {
            let current = relation.lookup(&change.key).map_err(|error| {
                BuiltinModelError(format!("read current source facts row: {error}"))
            })?;
            if current != change.expected {
                return Err(BuiltinModelError(
                    "source facts base does not match its persisted before value".to_owned(),
                ));
            }
        }
        let update = relation
            .prepare_update(&changes)
            .map_err(|error| BuiltinModelError(format!("prepare source facts delta: {error}")))?;
        let node_objects = update
            .changed_nodes()
            .iter()
            .map(|node| {
                TypedObject::from_state_root(node.commitment(), node).map_err(|error| {
                    BuiltinModelError(format!("retain source facts node: {error:?}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PreparedSourceFactsRelationUpdate {
            node_objects,
            pointer: Some(product_source_file_facts_root_object(
                update.target().root(),
            )),
        })
    } else {
        if intent
            .source_facts_changes()
            .iter()
            .any(|change| change.expected.is_some() || change.after.is_none())
        {
            return Err(BuiltinModelError(
                "source facts relation disappeared before its expected row".to_owned(),
            ));
        }
        let entries: Vec<([u8; 32], ProductSourceFileFactsRecord)> = intent
            .source_facts_changes()
            .iter()
            .filter_map(|change| change.after.clone().map(|record| (change.key, record)))
            .collect();
        let state = backend_engine::RelationState::<ProductSourceFileFactsRelation>::from_entries(
            entries,
            super::admitted_coverage()?,
        )
        .map_err(|error| {
            BuiltinModelError(format!("build initial source facts relation: {error}"))
        })?;
        let mut closure = state.node_closure();
        let mut node_objects = Vec::new();
        while let Some(node) = closure.try_next().map_err(|error| {
            BuiltinModelError(format!("traverse initial source facts nodes: {error:?}"))
        })? {
            node_objects.push(
                TypedObject::from_state_root(node.state_root(), node.canonical()).map_err(
                    |error| {
                        BuiltinModelError(format!("retain initial source facts node: {error:?}"))
                    },
                )?,
            );
        }
        Ok(PreparedSourceFactsRelationUpdate {
            node_objects,
            pointer: Some(product_source_file_facts_root_object(state.root())),
        })
    }
}

fn prepare_capture_relation_update(
    base: &WorkspaceSnapshot,
    intent: &BuiltinIntent,
    target_root: backend_engine::WorkspaceRoot,
    base_sequence: u64,
    request_identity: [u8; 32],
    source_commit: [u8; 32],
    semantic: &WorkspaceRelationHandle<BuiltinSemanticRelation>,
) -> Result<PreparedCaptureRelationUpdate, BuiltinModelError> {
    if intent.capture_changes().is_empty() {
        return Ok(PreparedCaptureRelationUpdate {
            node_objects: Vec::new(),
            pointer: None,
        });
    }
    let selected_capture_relation = semantic_capture_relation(base)
        .map_err(|error| BuiltinModelError(format!("open semantic capture relation: {error}")))?;
    let source_relation = if intent
        .capture_changes()
        .iter()
        .any(|change| change.compiler_failure.is_some())
    {
        Some(
            base.relation::<BuiltinWorkspaceRelation>()
                .map_err(|error| {
                    BuiltinModelError(format!("open source rows for compiler refusal: {error}"))
                })?,
        )
    } else {
        None
    };
    let mut capture_changes = Vec::with_capacity(intent.capture_changes().len());
    let mut capture_entries = Vec::with_capacity(intent.capture_changes().len());
    for change in intent.capture_changes() {
        if let Some(failure) = &change.compiler_failure {
            let project = change.key.package_key().to_bytes();
            let file_key =
                backend_engine::product_source_file_key(project, failure.relative_path());
            let selected_source_relation = source_relation.as_ref().ok_or_else(|| {
                BuiltinModelError("compiler failure has no selected source relation".to_owned())
            })?;
            let row = selected_source_relation
                .lookup(&file_key)
                .map_err(|error| {
                    BuiltinModelError(format!("read source row for compiler refusal: {error}"))
                })?
                .ok_or_else(|| {
                    BuiltinModelError(
                        "typed compiler refusal names a source path outside the captured project"
                            .to_owned(),
                    )
                })?;
            validate_compiler_failure_source(&change.key, failure, &row)?;
        }
        let current = selected_capture_relation
            .as_ref()
            .map(|relation| relation.lookup(&change.key))
            .transpose()
            .map_err(|error| BuiltinModelError(format!("read current semantic capture: {error}")))?
            .flatten();
        if current != change.expected {
            return Err(BuiltinModelError(
                "semantic capture base does not match its persisted before value".to_owned(),
            ));
        }
        let selected_prior = selected_semantic_prior(semantic, &change.key)?;
        let after = next_capture_record(
            current.as_ref(),
            change,
            selected_prior,
            request_identity,
            *base.root().as_bytes(),
            base_sequence,
            *target_root.as_bytes(),
            source_commit,
        )?;
        if let ProductSemanticCaptureOutcome::Published { coverage, claim } = after.outcome() {
            let selected = intent
                .semantic_changes()
                .iter()
                .find(|semantic_change| semantic_change.key == change.key)
                .and_then(|semantic_change| semantic_change.after.as_ref());
            if !selected.is_some_and(|record| {
                matches!(record, ProductSemanticPublicationRecord::Published {
                    coverage: selected_coverage,
                    claim: selected_claim,
                } if *selected_coverage == coverage && *selected_claim == claim)
            }) {
                return Err(BuiltinModelError(
                    "published semantic capture lacks its exact selected generation change"
                        .to_owned(),
                ));
            }
        }
        capture_entries.push((change.key.clone(), after.clone()));
        capture_changes.push(TreeChange {
            key: change.key.clone(),
            after: Some(after),
        });
    }
    if let Some(relation) = selected_capture_relation {
        let update = relation.prepare_update(&capture_changes).map_err(|error| {
            BuiltinModelError(format!("prepare semantic capture delta: {error}"))
        })?;
        let node_objects = update
            .changed_nodes()
            .iter()
            .map(|node| {
                TypedObject::from_state_root(node.commitment(), node).map_err(|error| {
                    BuiltinModelError(format!("retain semantic capture node: {error:?}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let pointer = semantic_capture_root_object(update.target().root());
        Ok(PreparedCaptureRelationUpdate {
            node_objects,
            pointer: Some(pointer),
        })
    } else {
        if intent
            .capture_changes()
            .iter()
            .any(|change| change.expected.is_some())
        {
            return Err(BuiltinModelError(
                "semantic capture pointer disappeared before its expected row".to_owned(),
            ));
        }
        let state = backend_engine::RelationState::<ProductSemanticCaptureRelation>::from_entries(
            capture_entries,
            super::admitted_coverage()?,
        )
        .map_err(|error| {
            BuiltinModelError(format!("build initial semantic capture relation: {error}"))
        })?;
        let mut closure = state.node_closure();
        let mut node_objects = Vec::new();
        while let Some(node) = closure.try_next().map_err(|error| {
            BuiltinModelError(format!(
                "traverse initial semantic capture nodes: {error:?}"
            ))
        })? {
            node_objects.push(
                TypedObject::from_state_root(node.state_root(), node.canonical()).map_err(
                    |error| {
                        BuiltinModelError(format!(
                            "retain initial semantic capture node: {error:?}"
                        ))
                    },
                )?,
            );
        }
        Ok(PreparedCaptureRelationUpdate {
            node_objects,
            pointer: Some(semantic_capture_root_object(state.root())),
        })
    }
}

fn selected_semantic_prior(
    semantic: &WorkspaceRelationHandle<BuiltinSemanticRelation>,
    key: &ProductSemanticPublicationKey,
) -> Result<Option<SemanticPublicationVersion>, BuiltinModelError> {
    let selected = semantic
        .lookup(key)
        .map_err(|error| BuiltinModelError(format!("read captured semantic selection: {error}")))?;
    Ok(selected.as_ref().and_then(|record| match record {
        ProductSemanticPublicationRecord::Published { coverage, claim } => {
            Some(SemanticPublicationVersion::new(*coverage, *claim))
        }
        ProductSemanticPublicationRecord::Unavailable(_) => None,
    }))
}

/// Applies the same capture state machine while preparing a live transition
/// and while re-admitting its persisted receipt. Persisted admission supplies
/// the intent's exact `expected` value as `current`, then compares this result
/// with the row reached through the authenticated target pointer.
fn next_capture_record(
    current: Option<&ProductSemanticCaptureRecord>,
    change: &BuiltinCaptureChange,
    selected_prior: Option<SemanticPublicationVersion>,
    request_identity: [u8; 32],
    base_workspace_root: [u8; 32],
    base_workspace_sequence: u64,
    source_workspace_root: [u8; 32],
    source_commit: [u8; 32],
) -> Result<ProductSemanticCaptureRecord, BuiltinModelError> {
    match (current, change.outcome) {
        (None, ProductSemanticCaptureOutcome::Pending { prior })
        | (Some(_), ProductSemanticCaptureOutcome::Pending { prior }) => {
            if current.is_some_and(|current| {
                matches!(
                    current.outcome(),
                    ProductSemanticCaptureOutcome::Pending { .. }
                )
            }) {
                return Err(BuiltinModelError(
                    "semantic capture refresh cannot replace an in-flight source marker".to_owned(),
                ));
            }
            if selected_prior != prior {
                return Err(BuiltinModelError(
                    "semantic capture prior does not match the selected coherent generation"
                        .to_owned(),
                ));
            }
            let source_workspace_sequence =
                base_workspace_sequence.checked_add(1).ok_or_else(|| {
                    BuiltinModelError("semantic capture workspace sequence overflow".to_owned())
                })?;
            ProductSemanticCaptureRecord::new_with_compiler_failure(
                change.capture.operation_key().copied(),
                request_identity,
                change.capture,
                base_workspace_root,
                base_workspace_sequence,
                source_workspace_root,
                source_workspace_sequence,
                source_commit,
                change.outcome,
                change.compiler_failure.clone(),
            )
            .map_err(|error| BuiltinModelError(error.to_owned()))
        }
        (Some(current), outcome) => {
            let ProductSemanticCaptureOutcome::Pending {
                prior: pending_prior,
            } = current.outcome()
            else {
                return Err(BuiltinModelError(
                    "semantic capture terminal has no pending source marker".to_owned(),
                ));
            };
            if current.capture() != change.capture {
                return Err(BuiltinModelError(
                    "semantic capture terminal does not match its exact pending source marker"
                        .to_owned(),
                ));
            }
            match outcome {
                ProductSemanticCaptureOutcome::Unavailable { .. } if pending_prior.is_some() => {
                    return Err(BuiltinModelError(
                        "unavailable semantic capture would discard its prior generation"
                            .to_owned(),
                    ));
                }
                ProductSemanticCaptureOutcome::Failed { prior, .. }
                    if Some(prior) != pending_prior =>
                {
                    return Err(BuiltinModelError(
                        "failed semantic capture changed its prior generation".to_owned(),
                    ));
                }
                ProductSemanticCaptureOutcome::Pending { .. } => {
                    return Err(BuiltinModelError(
                        "semantic capture refresh cannot replace an in-flight source marker"
                            .to_owned(),
                    ));
                }
                ProductSemanticCaptureOutcome::Unavailable { .. }
                | ProductSemanticCaptureOutcome::Failed { .. }
                | ProductSemanticCaptureOutcome::Published { .. } => {}
            }
            let terminal = current
                .with_outcome(outcome)
                .map_err(|error| BuiltinModelError(error.to_owned()))?;
            match &change.compiler_failure {
                Some(failure) => terminal
                    .with_compiler_failure(failure.clone())
                    .map_err(|error| BuiltinModelError(error.to_owned())),
                None => Ok(terminal),
            }
        }
        (None, _) => Err(BuiltinModelError(
            "semantic capture terminal has no exact pending source marker".to_owned(),
        )),
    }
}

fn validate_compiler_failure_source(
    key: &ProductSemanticPublicationKey,
    failure: &backend_library::PackageCompilerFailure,
    row: &BuiltinPackageRecord,
) -> Result<(), BuiltinModelError> {
    let fields = row.file_fields().ok_or_else(|| {
        BuiltinModelError(
            "typed compiler refusal path resolves to a non-file source row".to_owned(),
        )
    })?;
    let package = key.package_key().to_bytes();
    if fields.project != package
        || fields.path != failure.relative_path()
        || fields.source_identity != Some(failure.source_identity())
    {
        return Err(BuiltinModelError(
            "typed compiler refusal source identity does not match captured source".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn prepare_retired_fixture(
    base: &WorkspaceSnapshot,
    intent: &BuiltinIntent,
    transaction: TransactionId,
) -> Result<PreparedTransition, BuiltinModelError> {
    let relation = base
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open retired source fixture: {error}")))?;
    let update = prepare_source_update_with_layout(
        &relation,
        intent.package.to_bytes(),
        intent.changes(),
        SourceFileKeyLayout::Retired,
    )?;
    prepare_transition_with_source_update(base, intent, transaction, &relation, update)
}

/// This model exists only to certify a refused, authenticated historical
/// store head. It cannot plan an intent or be installed in a serving daemon.
#[derive(Clone, Debug)]
pub(super) struct RetiredSourceProbe {
    workspace: std::path::PathBuf,
}

impl RetiredSourceProbe {
    pub(super) fn new(workspace: &std::path::Path) -> Self {
        Self {
            workspace: workspace.to_owned(),
        }
    }

    // WorkspaceOwner opens/repairs its diagnostic journal after model and
    // store-pack admission. Refuse damaged diagnostics here, while its lease
    // is held, so upgrade classification never truncates or rewrites old bytes.
    fn admit_unmodified_diagnostics(&self) -> Result<(), BuiltinModelError> {
        let path = self.workspace.join("workspace.journal");
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(BuiltinModelError(error.to_string())),
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(BuiltinModelError(
                    "historical diagnostic journal is not a regular file".to_owned(),
                ));
            }
            Ok(_) => {}
        }
        let (_journal, scan) = backend_engine::HashChainJournal::<
            backend_engine::schema::WorkspaceLog,
        >::open_streaming_with_deferred_repair(
            path,
            backend_engine::JournalLimits::default(),
            |_| Ok(()),
        )
        .map_err(|error| BuiltinModelError(format!("historical diagnostic journal: {error}")))?;
        if scan.truncated_tail {
            return Err(BuiltinModelError(
                "historical diagnostic journal has a torn tail".to_owned(),
            ));
        }
        Ok(())
    }
}

impl WorkspaceModel for RetiredSourceProbe {
    type Intent = BuiltinIntent;
    type Error = BuiltinModelError;

    fn request_id(&self, intent: &Self::Intent) -> [u8; 32] {
        BuiltinModel.request_id(intent)
    }

    fn prepare(
        &self,
        _base: &WorkspaceSnapshot,
        _intent: &Self::Intent,
        _transaction: TransactionId,
    ) -> Result<PreparedTransition, Self::Error> {
        Err(BuiltinModelError(
            "retired source probe cannot publish".to_owned(),
        ))
    }

    fn admit_persisted(
        &self,
        _persisted: &backend_engine::PersistedTransition,
    ) -> Result<PreparedTransition, Self::Error> {
        Err(BuiltinModelError(
            "retired source probe requires the owned durable store".to_owned(),
        ))
    }

    fn admit_persisted_with_store(
        &self,
        persisted: &backend_engine::PersistedTransition,
        store: &backend_engine::FileStore,
    ) -> Result<PreparedTransition, Self::Error> {
        let transition =
            admit_persisted_transition_with_layout(persisted, store, SourceFileKeyLayout::Retired)?;
        self.admit_unmodified_diagnostics()?;
        Ok(transition)
    }
}

fn admit_persisted_transition(
    persisted: &backend_engine::PersistedTransition,
    store: &backend_engine::FileStore,
) -> Result<PreparedTransition, BuiltinModelError> {
    admit_persisted_transition_with_layout(persisted, store, SourceFileKeyLayout::Current)
}

fn admit_persisted_transition_with_layout(
    persisted: &backend_engine::PersistedTransition,
    store: &backend_engine::FileStore,
    layout: SourceFileKeyLayout,
) -> Result<PreparedTransition, BuiltinModelError> {
    let persisted_intent = admit_persisted_intent(persisted.closure_manifest().objects())?;
    if layout == SourceFileKeyLayout::Retired
        && BuiltinModel.request_id(&persisted_intent) != persisted.request()
    {
        return Err(BuiltinModelError(
            "retired persisted intent does not match its request identity".to_owned(),
        ));
    }
    let untrusted_manifest = WorkspaceManifest::decode_untrusted(persisted.manifest_bytes())
        .map_err(|error| BuiltinModelError(format!("decode persisted manifest: {error}")))?;

    // The rest of restart admission reopens both exact relation roots and
    // proves that this typed intent alone reproduces the authenticated delta.
    admit_persisted_relations(
        persisted,
        store,
        persisted_intent,
        untrusted_manifest,
        layout,
    )
}

fn admit_persisted_intent(objects: &[TypedObject]) -> Result<BuiltinIntent, BuiltinModelError> {
    let intent_schema = backend_version::SchemaIdentity::new(
        BuiltinIntentSchema::DOMAIN,
        BuiltinIntentSchema::TYPE,
        BuiltinIntentSchema::VERSION,
    );
    let mut intent_objects = objects
        .iter()
        .filter(|object| object.schema() == intent_schema);
    let intent_object = intent_objects.next().ok_or_else(|| {
        BuiltinModelError("persisted builtin transition has no package intent".to_owned())
    })?;
    if intent_objects.next().is_some() {
        return Err(BuiltinModelError(
            "persisted builtin transition has multiple package intents".to_owned(),
        ));
    }
    let intent_key = ObjectKey::<BuiltinIntentSchema>::from_value(intent_object.bytes());
    let intent_version = ObjectVersion::<BuiltinIntentSchema>::from_value(intent_object.bytes());
    if intent_object.key() != intent_key.as_bytes()
        || intent_object.version() != intent_version.as_bytes()
    {
        return Err(BuiltinModelError(
            "persisted builtin intent object identity does not match its bytes".to_owned(),
        ));
    }
    let persisted_intent = BuiltinIntent::decode(intent_object.bytes())
        .map_err(|error| BuiltinModelError(format!("decode persisted builtin intent: {error}")))?;
    // The object key is derived from these exact bytes, so an intent that
    // re-encodes differently cannot be replayed under its own identity. That
    // happens when the bytes were written by a build whose canonical form
    // differs from this one, which a reader can only resolve by re-indexing,
    // so the refusal says which of the two it is.
    if persisted_intent.encode().as_slice() != intent_object.bytes() {
        return Err(BuiltinModelError(
            "persisted builtin intent is not canonically encoded: this workspace was written \
             by a build with a different canonical encoding and has to be indexed again"
                .to_owned(),
        ));
    }
    Ok(persisted_intent)
}

#[expect(
    clippy::too_many_lines,
    reason = "admission keeps the authenticated persisted transition phases together"
)]
fn admit_persisted_relations(
    persisted: &backend_engine::PersistedTransition,
    store: &backend_engine::FileStore,
    persisted_intent: BuiltinIntent,
    untrusted_manifest: backend_version::UntrustedWorkspaceManifest,
    layout: SourceFileKeyLayout,
) -> Result<PreparedTransition, BuiltinModelError> {
    let target_root = untrusted_manifest
        .relations()
        .iter()
        .find(|binding| {
            binding.schema()
                == backend_version::SchemaIdentity::of_relation::<BuiltinWorkspaceRelation>()
        })
        .map(|binding| binding.root())
        .ok_or_else(|| {
            BuiltinModelError("persisted builtin manifest has no relation".to_owned())
        })?;
    let target_semantic_root = untrusted_manifest
        .relations()
        .iter()
        .find(|binding| {
            binding.schema()
                == backend_version::SchemaIdentity::of_relation::<BuiltinSemanticRelation>()
        })
        .map(|binding| binding.root())
        .ok_or_else(|| {
            BuiltinModelError("persisted builtin manifest has no semantic relation".to_owned())
        })?;
    let target_tree = persisted
        .relation::<BuiltinWorkspaceRelation>(store, target_root)
        .map_err(|error| BuiltinModelError(format!("open persisted target relation: {error}")))?;
    let target_semantic = persisted
        .relation::<BuiltinSemanticRelation>(store, target_semantic_root)
        .map_err(|error| {
            BuiltinModelError(format!("open persisted target semantic relation: {error}"))
        })?;
    let delta_header = persisted
        .delta_header()
        .map_err(|error| BuiltinModelError(format!("decode persisted delta header: {error}")))?;
    let base_root = delta_header
        .relations()
        .iter()
        .find(|relation| {
            relation.schema()
                == backend_version::SchemaIdentity::of_relation::<BuiltinWorkspaceRelation>()
        })
        .map_or(target_root, |relation| relation.base());
    let base_semantic_root = delta_header
        .relations()
        .iter()
        .find(|relation| {
            relation.schema()
                == backend_version::SchemaIdentity::of_relation::<BuiltinSemanticRelation>()
        })
        .map_or(target_semantic_root, |relation| relation.base());
    let base_tree = persisted
        .relation::<BuiltinWorkspaceRelation>(store, base_root)
        .map_err(|error| BuiltinModelError(format!("open persisted base relation: {error}")))?;
    let base_semantic = persisted
        .relation::<BuiltinSemanticRelation>(store, base_semantic_root)
        .map_err(|error| {
            BuiltinModelError(format!("open persisted base semantic relation: {error}"))
        })?;
    persisted_intent.admit_semantic_selection_against(&base_semantic)?;
    let update = prepare_source_update_with_layout(
        &base_tree,
        persisted_intent.package.to_bytes(),
        persisted_intent.changes(),
        layout,
    )?;
    let semantic_update =
        prepare_semantic_update(&base_semantic, persisted_intent.semantic_changes())?;
    let source_change_count = update.delta().changes().count();
    let semantic_change_count = semantic_update.delta().changes().count();
    let source_changed = source_change_count != 0;
    let semantic_changed = semantic_change_count != 0;
    let changed_items = checked_product_change_count(
        source_change_count,
        semantic_change_count,
        &persisted_intent,
    )?;
    if changed_items == 0 {
        return Err(BuiltinModelError(
            "persisted product transition is a no-op".to_owned(),
        ));
    }
    let work = TransitionWork::from_lazy(changed_items, update.work())
        .map_err(|error| BuiltinModelError(format!("admit persisted work: {error}")))?;
    if update.target().root().to_bytes() != target_root {
        return Err(BuiltinModelError(
            "persisted builtin target root does not match its package change".to_owned(),
        ));
    }
    if semantic_update.target().root().to_bytes() != target_semantic_root {
        return Err(BuiltinModelError(
            "persisted semantic target root does not match its publication change".to_owned(),
        ));
    }
    let target_manifest =
        workspace_manifest_from_root(&target_tree.root_handle(), &target_semantic.root_handle())
            .map_err(|error| BuiltinModelError(format!("rebuild target manifest: {error}")))?;
    let base_manifest =
        workspace_manifest_from_root(&base_tree.root_handle(), &base_semantic.root_handle())
            .map_err(|error| BuiltinModelError(format!("rebuild base manifest: {error}")))?;
    let relation_delta = update.delta();
    let semantic_delta = semantic_update.delta();
    let manifest = admit_manifest(untrusted_manifest, &target_manifest)
        .map_err(|error| BuiltinModelError(format!("admit persisted manifest: {error}")))?;
    if manifest != target_manifest {
        return Err(BuiltinModelError(
            "persisted builtin manifest changed during typed admission".to_owned(),
        ));
    }

    let delta = delta_header
        .admit_encoded(
            persisted.delta_bytes(),
            &base_manifest,
            &manifest,
            [
                source_changed.then(|| RelationTransition::from_delta(&relation_delta)),
                semantic_changed.then(|| RelationTransition::from_delta(&semantic_delta)),
            ]
            .into_iter()
            .flatten()
            .collect(),
        )
        .map_err(|error| BuiltinModelError(format!("admit persisted delta: {error}")))?;

    let untrusted_commit = Commit::decode_untrusted(persisted.commit_bytes())
        .map_err(|error| BuiltinModelError(format!("decode persisted commit: {error}")))?;
    let authority = ObjectClosure::from_version(
        ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE),
    );
    let transaction = ObjectClosure::from_version(persisted.transaction().version());
    let provenance = untrusted_commit
        .provenance()
        .clone()
        .admit(authority, transaction)
        .map_err(|error| BuiltinModelError(format!("admit persisted provenance: {error}")))?;
    if provenance.detail() != persisted.request() {
        return Err(BuiltinModelError(
            "persisted builtin commit detail does not match its request".to_owned(),
        ));
    }
    let commit = untrusted_commit
        .admit(&manifest, provenance)
        .map_err(|error| BuiltinModelError(format!("admit persisted commit: {error}")))?;
    let checked_delta = delta.clone().into_checked();
    let checked_commit = commit.clone().into_checked();
    let registry = super::product_relation_registry()?;
    // Reopen the exact authenticated closure selected by the physical HEAD.
    // The compact transition envelope intentionally contains only recovery
    // pointers, so rebuilding a new manifest from that subset would produce a
    // different closure identity. The durable manifest remains content
    // addressed and is admitted through the store's relation registry here.
    let selected = store
        .head()
        .map_err(|error| BuiltinModelError(format!("read selected workspace head: {error:?}")))?
        .ok_or_else(|| BuiltinModelError("selected workspace head disappeared".to_owned()))?;
    // The workspace closure is published root-only — `write_workspace_root_closure`
    // admits the retained objects and leaves their relation children in the
    // node store, because a path-copying tree does not re-list untouched
    // canonical nodes in every closure. Reading it back through the
    // complete-closure rule therefore rejects every workspace whose relation
    // tree needed an internal node: a one-leaf project reopened, and a real
    // crate did not. Read it with the reader that matches the writer.
    let closure_id = selected.descriptor().closure();
    let persisted_objects = store
        .read_workspace_root_closure(closure_id)
        .map_err(|error| {
            BuiltinModelError(format!(
                "read selected workspace closure {}: {error:?}",
                backend_engine::encode_id(closure_id.as_bytes())
            ))
        })?;
    validate_persisted_capture_changes(
        persisted,
        store,
        &persisted_intent,
        &base_tree,
        &target_tree,
        &base_semantic,
        &target_semantic,
        selected.descriptor().target_generation(),
        commit.id().as_bytes(),
        persisted_objects.objects(),
    )?;
    validate_persisted_source_facts(
        persisted,
        store,
        &persisted_intent,
        &target_tree,
        persisted_objects.objects(),
    )?;
    let closure = WorkspaceClosure::from_checked_transition_root_only_with_registry(
        &manifest,
        &checked_delta,
        Some(&checked_commit),
        persisted_objects,
        &registry,
    )
    .map_err(|error| BuiltinModelError(format!("admit persisted closure: {error:?}")))?;
    if layout == SourceFileKeyLayout::Retired {
        super::read_indexed_relation(&base_tree, layout)?;
        let sources = super::read_indexed_relation(&target_tree, layout)?;
        if sources.files.is_empty() {
            return Err(BuiltinModelError(
                "retired layout has no retired source files".to_owned(),
            ));
        }
    }
    let intent_bytes = persisted_intent.encode();
    let intent_key = ObjectKey::<BuiltinIntentSchema>::from_value(&intent_bytes);
    let intent_object = TypedObject::from_value(&intent_key, &intent_bytes);
    let transition = PreparedTransition::new_with_registry(
        persisted.request(),
        persisted.transaction(),
        delta,
        commit,
        closure,
        &registry,
    )
    .map_err(|error| BuiltinModelError(format!("rebuild persisted transition: {error}")))?;
    transition
        .replace_object_family([intent_object], &registry)
        .map(|transition| transition.with_work(work))
        .map_err(|error| BuiltinModelError(format!("retain persisted intent: {error}")))
}

fn validate_persisted_source_facts(
    persisted: &backend_engine::PersistedTransition,
    store: &backend_engine::FileStore,
    intent: &BuiltinIntent,
    source: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    closure_objects: &[TypedObject],
) -> Result<(), BuiltinModelError> {
    if intent.source_facts_changes().is_empty() {
        return Ok(());
    }
    let pointer_schema = backend_version::SchemaIdentity::new(
        ProductSourceFileFactsRootSchema::DOMAIN,
        ProductSourceFileFactsRootSchema::TYPE,
        ProductSourceFileFactsRootSchema::VERSION,
    );
    let pointer_key = ObjectKey::<ProductSourceFileFactsRootSchema>::from_value(&[0x46; 32]);
    let pointer = closure_objects
        .iter()
        .find(|object| object.schema() == pointer_schema && object.key() == pointer_key.as_bytes())
        .ok_or_else(|| {
            BuiltinModelError("persisted source facts update has no target root pointer".to_owned())
        })?;
    let root: [u8; 32] = pointer
        .bytes()
        .try_into()
        .map_err(|_| BuiltinModelError("malformed persisted source facts root".to_owned()))?;
    let target = persisted
        .relation::<ProductSourceFileFactsRelation>(store, root)
        .map_err(|error| {
            BuiltinModelError(format!("open persisted target source facts: {error}"))
        })?;
    for change in intent.source_facts_changes() {
        let after = target.lookup(&change.key).map_err(|error| {
            BuiltinModelError(format!("read persisted target source facts: {error}"))
        })?;
        if after != change.after {
            return Err(BuiltinModelError(
                "persisted source facts root does not match its exact intent rows".to_owned(),
            ));
        }
        if let Some(ProductSourceFileFactsRecord::Manifest(manifest)) = &after {
            let source_record = source
                .lookup(&change.key)
                .map_err(|error| {
                    BuiltinModelError(format!("read persisted target source row: {error}"))
                })?
                .ok_or_else(|| {
                    BuiltinModelError(
                        "persisted source facts manifest has no source row".to_owned(),
                    )
                })?;
            let file = source_record.file_fields().ok_or_else(|| {
                BuiltinModelError("persisted source facts owner is not a file".to_owned())
            })?;
            admit_product_source_file_facts(file, change.key, manifest.clone(), |key| {
                target.lookup(key).map_err(|error| error.to_string())
            })
            .map_err(|error| {
                BuiltinModelError(format!("admit persisted complete source facts: {error}"))
            })?;
        }
    }
    Ok(())
}

fn validate_persisted_capture_changes(
    persisted: &backend_engine::PersistedTransition,
    store: &backend_engine::FileStore,
    intent: &BuiltinIntent,
    base_source: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    target_source: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    base_semantic: &WorkspaceRelationHandle<BuiltinSemanticRelation>,
    target_semantic: &WorkspaceRelationHandle<BuiltinSemanticRelation>,
    target_sequence: u64,
    target_commit: &[u8; 32],
    closure_objects: &[TypedObject],
) -> Result<(), BuiltinModelError> {
    if intent.capture_changes().is_empty() {
        return Ok(());
    }
    let pointer_schema = backend_version::SchemaIdentity::new(
        ProductSemanticCaptureRootSchema::DOMAIN,
        ProductSemanticCaptureRootSchema::TYPE,
        ProductSemanticCaptureRootSchema::VERSION,
    );
    let pointer_key = ObjectKey::<ProductSemanticCaptureRootSchema>::from_value(&[0x53; 32]);
    let mut pointers = closure_objects
        .iter()
        .filter(|object| object.schema() == pointer_schema);
    let pointer = pointers.next().ok_or_else(|| {
        BuiltinModelError("persisted capture update has no target root pointer".to_owned())
    })?;
    if pointers.next().is_some() || pointer.key() != pointer_key.as_bytes() {
        return Err(BuiltinModelError(
            "persisted capture target has an ambiguous or noncanonical root pointer".to_owned(),
        ));
    }
    let capture_root: [u8; 32] = pointer
        .bytes()
        .try_into()
        .map_err(|_| BuiltinModelError("malformed persisted capture root".to_owned()))?;
    let target_capture = persisted
        .relation::<ProductSemanticCaptureRelation>(store, capture_root)
        .map_err(|error| {
            BuiltinModelError(format!("open persisted target semantic capture: {error}"))
        })?;
    let base_manifest =
        workspace_manifest_from_root(&base_source.root_handle(), &base_semantic.root_handle())?;
    let target_manifest =
        workspace_manifest_from_root(&target_source.root_handle(), &target_semantic.root_handle())?;
    let base_sequence = target_sequence.checked_sub(1).ok_or_else(|| {
        BuiltinModelError("persisted capture target sequence has no base".to_owned())
    })?;
    for change in intent.capture_changes() {
        if !matches!(
            change.outcome,
            ProductSemanticCaptureOutcome::Pending { .. }
        ) && let Some(expected) = &change.expected
        {
            if !matches!(
                expected.outcome(),
                ProductSemanticCaptureOutcome::Pending { .. }
            ) || expected.capture() != change.capture
                || expected.source_workspace_root() != base_manifest.root().as_bytes()
                || expected.source_workspace_sequence() != base_sequence
            {
                return Err(BuiltinModelError(
                    "persisted semantic terminal does not match its exact before receipt"
                        .to_owned(),
                ));
            }
        }
        let selected_prior = selected_semantic_prior(base_semantic, &change.key)?;
        let after = next_capture_record(
            change.expected.as_ref(),
            change,
            selected_prior,
            persisted.request(),
            *base_manifest.root().as_bytes(),
            base_sequence,
            *target_manifest.root().as_bytes(),
            *target_commit,
        )?;
        let actual = target_capture
            .lookup(&change.key)
            .map_err(|error| {
                BuiltinModelError(format!("read persisted target semantic capture: {error}"))
            })?
            .ok_or_else(|| {
                BuiltinModelError("persisted capture root omits its exact intent row".to_owned())
            })?;
        if actual != after {
            return Err(BuiltinModelError(
                "persisted capture root does not match its exact before receipt and terminal"
                    .to_owned(),
            ));
        }
        if let Some(failure) = &change.compiler_failure {
            let project = change.key.package_key().to_bytes();
            let file_key =
                backend_engine::product_source_file_key(project, failure.relative_path());
            let row = target_source
                .lookup(&file_key)
                .map_err(|error| {
                    BuiltinModelError(format!("read terminal capture source row: {error}"))
                })?
                .ok_or_else(|| {
                    BuiltinModelError(
                        "typed compiler refusal names a source path outside the captured project"
                            .to_owned(),
                    )
                })?;
            validate_compiler_failure_source(&change.key, failure, &row)?;
        }
        let selected_target_prior = selected_semantic_prior(target_semantic, &change.key)?;
        match change.outcome {
            ProductSemanticCaptureOutcome::Pending { prior }
                if selected_prior != prior || selected_target_prior != prior =>
            {
                return Err(BuiltinModelError(
                    "persisted pending capture does not match the selected semantic prior"
                        .to_owned(),
                ));
            }
            ProductSemanticCaptureOutcome::Unavailable { .. }
                if selected_target_prior.is_some() =>
            {
                return Err(BuiltinModelError(
                    "unavailable capture changed a coherent semantic selection".to_owned(),
                ));
            }
            ProductSemanticCaptureOutcome::Failed { prior, .. }
                if selected_target_prior != Some(prior) =>
            {
                return Err(BuiltinModelError(
                    "failed capture did not retain its exact semantic prior".to_owned(),
                ));
            }
            ProductSemanticCaptureOutcome::Published { coverage, claim } => {
                let selected = target_semantic.lookup(&change.key).map_err(|error| {
                    BuiltinModelError(format!("read published capture selection: {error}"))
                })?;
                if !selected.is_some_and(|record| {
                    matches!(record, ProductSemanticPublicationRecord::Published {
                        coverage: selected_coverage,
                        claim: selected_claim,
                    } if selected_coverage == coverage && selected_claim == claim)
                }) {
                    return Err(BuiltinModelError(
                        "published capture does not match the selected semantic generation"
                            .to_owned(),
                    ));
                }
            }
            ProductSemanticCaptureOutcome::Pending { .. }
            | ProductSemanticCaptureOutcome::Unavailable { .. }
            | ProductSemanticCaptureOutcome::Failed { .. } => {}
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct BuiltinIntentSchema;

impl Schema for BuiltinIntentSchema {
    const DOMAIN: u8 = 0x96;
    const TYPE: u16 = 3;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Output/CAS adapter for the compiled profile.  The validator checks the
/// exact bytes and returns retaining publication evidence, including the
/// shared owner used by remote admission.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BuiltinOutputValidator;

impl backend_engine::OutputAdmissionValidator for BuiltinOutputValidator {
    fn validate(
        &self,
        output: OutputVersion,
        bytes: &[u8],
        semantic: &CompleteSemanticCoverage,
    ) -> Result<backend_engine::RetainedOutput, DispatchError> {
        builtin_output_check(output, bytes, semantic)?;
        Ok(backend_engine::RetainedOutput::from_bytes(output, bytes))
    }

    fn validate_shared(
        &self,
        output: OutputVersion,
        bytes: &Arc<Vec<u8>>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<backend_engine::RetainedOutput, DispatchError> {
        builtin_output_check(output, bytes.as_slice(), semantic)?;
        Ok(backend_engine::RetainedOutput::from_shared(
            output,
            Arc::clone(bytes),
        ))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BuiltinSemanticAuthority {
    profile: Arc<ProfileDescriptor>,
}
pub(crate) type BuiltinValidator =
    CompositeAdmissionValidator<BuiltinOutputValidator, BuiltinSemanticAuthority>;
pub(crate) type BuiltinAuthorityVerifier = Blake3AuthorityVerifier;

pub(super) fn builtin_output_check(
    output: OutputVersion,
    bytes: &[u8],
    semantic: &CompleteSemanticCoverage,
) -> Result<(), DispatchError> {
    let is_product = bytes.len()
        == PRODUCT_OUTPUT_BYTES.len() + backend_engine::SEMANTIC_OUTPUT_BODY_BYTES
        && bytes.starts_with(PRODUCT_OUTPUT_BYTES);
    let is_echo = bytes == ECHO_OUTPUT_BYTES;
    if !semantic.is_complete()
        || (!is_product && !is_echo)
        || OutputVersion::from_value(bytes) != output
    {
        return Err(DispatchError::OutputMismatch);
    }
    Ok(())
}

impl SemanticCoverageValidator for BuiltinSemanticAuthority {
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        backend_engine::builtin::validate_semantic_claim(self.profile.ids, binding, claim)
    }

    fn validate_manifest(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
        manifest: &DependencyManifest,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        backend_engine::builtin::validate_semantic_manifest(
            self.profile.ids,
            binding,
            claim,
            manifest,
        )
    }
}

pub(super) fn builtin_validator(profile: Arc<ProfileDescriptor>) -> BuiltinValidator {
    CompositeAdmissionValidator::new(BuiltinOutputValidator, BuiltinSemanticAuthority { profile })
}

pub(super) fn builtin_dispatcher(
    product_secret: Option<[u8; 32]>,
    profile: Arc<ProfileDescriptor>,
    attempt_lease_ticks: u64,
) -> Result<Dispatcher<BuiltinValidator, BuiltinAuthorityVerifier>, String> {
    let ids = profile.ids;
    let envelope = execution_resources(ids);
    let max_output = profile_output_len(ids) as u64;
    let budget = Budget {
        operations: 64,
        bytes: 64 * 1024,
        hedges: 0,
        resources: ResourceVector {
            cpu_millis: 64_u64.saturating_mul(envelope.cpu_millis),
            memory_bytes: 64_u64.saturating_mul(envelope.memory_bytes),
            network_bytes: 64 * max_output,
            storage_bytes: 64 * max_output,
        },
    };
    let scheduler = Scheduler::with_envelopes_and_lease_duration(
        backend_engine::EnvelopeBudgets::uniform(budget),
        attempt_lease_ticks,
    );
    let mut authority = Blake3AuthorityVerifier::new();
    match profile.kind {
        BuiltinProfile::Product => {
            let secret = product_secret.ok_or_else(|| {
                "production profile requires a configured authority verifier".to_owned()
            })?;
            authority.insert_key(ids.authority.to_bytes(), secret);
        }
        BuiltinProfile::EchoFixture => {
            authority.insert_key(ids.authority.to_bytes(), ECHO_AUTHORITY_SECRET);
        }
    }
    Ok(Dispatcher::with_scheduler(
        scheduler,
        builtin_validator(profile),
        authority,
        RemoteAuthorityPolicy::Signed,
    ))
}

#[cfg(test)]
mod persisted_intent_tests {
    use super::*;
    use backend_engine::publication::{
        binding::{COMPILATION_BINDING_BYTES, CompilationBindingView},
        manifest::{CompilationManifestFacts, CompilationManifestFormat},
    };
    use backend_store::hydration::VerifiedGenerationFacts;
    use backend_version::{
        ContentId, DependencySetDomain, GenerationId, IrManifestDomain, IrManifestEncoding,
    };

    #[derive(Debug)]
    struct ForeignSchema;

    impl Schema for ForeignSchema {
        const DOMAIN: u8 = 0xfe;
        const TYPE: u16 = 1;
        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    fn intent_object(intent: &BuiltinIntent) -> TypedObject {
        let bytes = intent.encode();
        let key = ObjectKey::<BuiltinIntentSchema>::from_value(&bytes);
        TypedObject::from_value(&key, &bytes)
    }

    fn semantic_claim(seed: &[u8]) -> SemanticPublicationClaim {
        let generation = VerifiedGenerationFacts {
            pinned_root: GenerationId::from_canonical_bytes(seed),
            dep_set: ContentId::<DependencySetDomain>::from_canonical_bytes(
                b"persisted-selection-dependencies",
            ),
        };
        let manifest =
            backend_version::ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(
                b"persisted-selection-manifest",
            );
        let mut binding_bytes = [0_u8; COMPILATION_BINDING_BYTES];
        let binding = CompilationBindingView::write_into(generation, manifest, &mut binding_bytes)
            .expect("selection binding");
        SemanticPublicationClaim::admit(
            CompilationManifestFacts {
                identity: manifest,
                format: CompilationManifestFormat::SemanticV2,
                fragment_count: 1,
                byte_length: 1,
            },
            *binding,
        )
        .expect("selection claim")
    }

    fn semantic_selection_fixture() -> (
        backend_engine::PackageKey,
        ProductSemanticPublicationKey,
        ProductSemanticPublicationKey,
        ProductSemanticPublicationRecord,
        ProductSemanticPublicationRecord,
    ) {
        let label = "pkg:cargo/persisted-selection@1.0.0";
        let package = backend_engine::PackageKey::from_value(label);
        let package_reference = backend_engine::PackageReference::parse(label.to_owned())
            .expect("selection package reference");
        let coordinate = backend_semantic::vocabulary::PackageUrl::parse(label.to_owned())
            .expect("selection coordinate");
        let selected = ProductSemanticPublicationKey::new(
            package_reference,
            coordinate,
            backend_semantic::vocabulary::LanguageProfile::Rust(
                backend_semantic::vocabulary::RustEdition::Rust2024,
            ),
        )
        .expect("selection key");
        let claim = semantic_claim(b"persisted-selection-generation");
        let generation = selected.for_generation(claim.binding().identity);
        let before = ProductSemanticPublicationRecord::Unavailable(
            backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority,
        );
        let after = ProductSemanticPublicationRecord::Published {
            coverage: backend_engine::builtin::SemanticPublicationCoverage::Complete,
            claim,
        };
        (package, selected, generation, before, after)
    }

    /// Builds an index intent carrying one source file record.
    fn file_intent(declarations: Vec<backend_compile::SourceDeclaration>) -> BuiltinIntent {
        let label = "fixture:containment";
        let package = backend_engine::PackageKey::from_value(label);
        let key = backend_engine::product_source_file_key(package.to_bytes(), "src/lib.rs");
        let record = BuiltinPackageRecord::file(
            package.to_bytes(),
            "src/lib.rs",
            backend_engine::SourceLanguage::Rust,
            [4; 32],
            [5; 32],
            Arc::from(declarations.into_boxed_slice()),
        )
        .expect("file record");
        BuiltinIntent::index_with_semantics(
            package,
            label,
            vec![BuiltinSourceChange {
                key,
                after: Some(record),
            }],
            Vec::new(),
        )
        .expect("file intent")
    }

    fn typed_compiler_failure() -> backend_library::PackageCompilerFailure {
        use backend_library::interface::{CompilerFragmentFailure, SourceAuthority};
        use backend_semantic::ir::{BuildError, EntityId};
        use backend_version::{CompileRecipeDomain, SourceFactDomain};

        let attempt = backend_library::interface::CompilerAttempt {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"source bytes"),
                byte_len: 12,
            },
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"recipe bytes"),
        };
        let failure = CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
            owner: EntityId::new(7),
            start: 18,
            end: 24,
        });
        backend_library::PackageCompilerFailure::from_fragment_failure(
            "src/lib.rs",
            attempt,
            &failure,
        )
        .expect("bounded package compiler failure")
    }

    #[test]
    fn keyed_index_intents_round_trip_and_have_distinct_workspace_request_identities() {
        let intent = file_intent(vec![declaration(backend_compile::Container::Module)]);
        assert!(intent.encode().starts_with(b"BPI4"));
        let first = intent
            .clone()
            .with_operation_key(
                backend_library::IndexOperationKey::from_bytes([1; 32]).expect("first key"),
            )
            .expect("bind first operation key");
        let second = intent
            .with_operation_key(
                backend_library::IndexOperationKey::from_bytes([2; 32]).expect("second key"),
            )
            .expect("bind second operation key");
        let encoded = first.encode();
        assert!(encoded.starts_with(b"BPI5"));
        assert_eq!(
            BuiltinIntent::decode(&encoded).expect("decode keyed intent"),
            first
        );
        assert_ne!(
            BuiltinModel.request_id(&first),
            BuiltinModel.request_id(&second),
            "caller key must be bound into the exact workspace request identity",
        );
    }

    #[test]
    fn typed_compiler_refusal_intent_round_trips_canonically() {
        let label = "pkg:cargo/persisted-selection@1.0.0";
        let package = backend_engine::PackageKey::from_value(label);
        let package_reference = backend_engine::PackageReference::parse(label.to_owned())
            .expect("compiler refusal package reference");
        let coordinate = backend_semantic::vocabulary::PackageUrl::parse(label.to_owned())
            .expect("compiler refusal coordinate");
        let key = ProductSemanticPublicationKey::new(
            package_reference,
            coordinate,
            backend_semantic::vocabulary::LanguageProfile::Rust(
                backend_semantic::vocabulary::RustEdition::Rust2024,
            ),
        )
        .expect("compiler refusal key");
        let capture =
            SemanticSourceCapture::new(None, [5; 32], [6; 32], 1, 1).expect("source capture");
        let intent = BuiltinIntent::index_with_capture(
            package,
            label,
            Vec::new(),
            Vec::new(),
            vec![BuiltinCaptureChange {
                key,
                expected: None,
                capture,
                outcome: ProductSemanticCaptureOutcome::Unavailable {
                    reason: backend_engine::builtin::SemanticUnavailableReason::Rejected,
                },
                compiler_failure: Some(typed_compiler_failure()),
            }],
        )
        .expect("typed refusal intent");
        let encoded = intent.encode();
        assert!(encoded.starts_with(b"BPI8"));
        assert_eq!(
            BuiltinIntent::decode(&encoded).expect("decode BPI8"),
            intent
        );

        let mut noncanonical = encoded;
        *noncanonical.last_mut().expect("failure json byte") ^= 1;
        assert!(BuiltinIntent::decode(&noncanonical).is_err());
    }

    fn declaration(container: backend_compile::Container) -> backend_compile::SourceDeclaration {
        backend_compile::SourceDeclaration::at_path(
            "src/lib.rs",
            "answer",
            "function",
            7,
            "fn answer() -> u32",
            "",
        )
        .expect("declaration")
        .with_container(container)
    }

    /// A workspace written before containment existed must still open.
    ///
    /// Restart admission re-encodes the persisted intent and refuses it
    /// unless the bytes are identical, and the intent embeds whole source
    /// records. A record that states no containment therefore has to keep the
    /// format tag it was written with; bumping it unconditionally refused
    /// every workspace indexed by an earlier binary with
    /// "persisted builtin intent is not canonically encoded".
    #[test]
    fn a_persisted_intent_without_containment_keeps_its_earlier_encoding() {
        let intent = file_intent(vec![declaration(backend_compile::Container::Module)]);
        let bytes = intent.encode();
        assert!(bytes.windows(4).any(|window| window == b"PSR8"));
        assert!(!bytes.windows(4).any(|window| window == b"PSR9"));
        let object = intent_object(&intent);
        assert_eq!(
            admit_persisted_intent(std::slice::from_ref(&object)).expect("admit intent"),
            intent
        );
    }

    #[test]
    fn a_persisted_intent_with_containment_is_admitted_and_canonical() {
        let intent = file_intent(vec![declaration(backend_compile::Container::attached(
            "Worker",
        ))]);
        let bytes = intent.encode();
        assert!(bytes.windows(4).any(|window| window == b"PSR9"));
        let object = intent_object(&intent);
        let admitted = admit_persisted_intent(std::slice::from_ref(&object)).expect("admit intent");
        assert_eq!(admitted, intent);
        assert_eq!(admitted.encode(), bytes);
    }

    #[test]
    fn persisted_intent_admission_is_schema_exact_unique_and_canonical() {
        let first_label = "fixture:first";
        let first = BuiltinIntent::add(
            backend_engine::PackageKey::from_value(first_label),
            first_label,
        )
        .expect("first intent");
        let first_object = intent_object(&first);
        assert_eq!(
            admit_persisted_intent(std::slice::from_ref(&first_object)).expect("admit intent"),
            first
        );

        let wrong_key_bytes = first.encode();
        let wrong_key = ObjectKey::<BuiltinIntentSchema>::from_value(b"wrong-persisted-key");
        let wrong_key_object = TypedObject::from_value(&wrong_key, &wrong_key_bytes);
        assert!(
            admit_persisted_intent(&[wrong_key_object])
                .expect_err("wrong persisted intent key")
                .to_string()
                .contains("identity does not match")
        );

        let mut wrong_encoding_version = first.encode();
        wrong_encoding_version[4] = 3;
        let wrong_encoding_version_key =
            ObjectKey::<BuiltinIntentSchema>::from_value(&wrong_encoding_version);
        let wrong_encoding_version_object =
            TypedObject::from_value(&wrong_encoding_version_key, &wrong_encoding_version);
        assert!(
            admit_persisted_intent(&[wrong_encoding_version_object])
                .expect_err("wrong persisted intent encoding version")
                .to_string()
                .contains("decode persisted builtin intent")
        );

        let mut noncanonical = first.encode();
        noncanonical.push(0);
        let noncanonical_key = ObjectKey::<BuiltinIntentSchema>::from_value(&noncanonical);
        let noncanonical_object = TypedObject::from_value(&noncanonical_key, &noncanonical);
        assert!(
            admit_persisted_intent(&[noncanonical_object])
                .expect_err("trailing persisted intent bytes")
                .to_string()
                .contains("decode persisted builtin intent")
        );

        let foreign_bytes = first.encode();
        let foreign_key = ObjectKey::<ForeignSchema>::from_value(&foreign_bytes);
        let foreign = TypedObject::from_value(&foreign_key, &foreign_bytes);
        assert!(
            admit_persisted_intent(&[foreign])
                .expect_err("foreign schema")
                .to_string()
                .contains("no package intent")
        );

        let second_label = "fixture:second";
        let second = BuiltinIntent::add(
            backend_engine::PackageKey::from_value(second_label),
            second_label,
        )
        .expect("second intent");
        assert!(
            admit_persisted_intent(&[first_object.clone(), intent_object(&second)])
                .expect_err("duplicate intent family")
                .to_string()
                .contains("multiple package intents")
        );

        let malformed = b"not-a-builtin-intent".as_slice();
        let malformed_key = ObjectKey::<BuiltinIntentSchema>::from_value(malformed);
        let malformed = TypedObject::from_value(&malformed_key, malformed);
        assert!(
            admit_persisted_intent(&[malformed])
                .expect_err("malformed intent")
                .to_string()
                .contains("decode persisted builtin intent")
        );

        // The object version is authenticated by the durable closure wire
        // boundary before local-service receives its typed object list.
        let closure =
            backend_engine::ClosureManifest::new(vec![first_object]).expect("valid intent closure");
        let mut closure_bytes = closure.encode(1 << 20).expect("encode intent closure");
        let object_version = b"LUNA_CLOSURE_V1\0".len() + 32 + 4 + 4 + 32;
        closure_bytes[object_version] ^= 1;
        assert!(
            backend_engine::ClosureManifest::decode(&closure_bytes, 1 << 20).is_err(),
            "a forged object version must fail before persisted-intent admission"
        );
    }

    #[test]
    fn persisted_selection_intent_round_trips_zero_source_delta_and_rejects_history_mismatch() {
        let (package, selected, generation, before, after) = semantic_selection_fixture();
        let label = "pkg:cargo/persisted-selection@1.0.0";
        let intent = BuiltinIntent::select_semantic_generation(
            package,
            label,
            selected.clone(),
            generation.clone(),
            Some(before.clone()),
            after.clone(),
        )
        .expect("selection intent");
        assert!(intent.changes().is_empty());
        assert_eq!(
            BuiltinIntent::decode(&intent.encode()).expect("selection intent roundtrip"),
            intent
        );

        // A selection cannot hide a no-op under a different history claim:
        // before and after are retained and must differ exactly once.
        let same_before = BuiltinIntent::select_semantic_generation(
            package,
            label,
            selected.clone(),
            generation.clone(),
            Some(after.clone()),
            after.clone(),
        )
        .expect_err("same before/after selection");
        assert!(
            same_before
                .to_string()
                .contains("one exact before-to-after")
        );

        // A generation key is immutable history. A claim from another
        // generation must fail admission even when its display coordinate is
        // identical.
        let other_claim = semantic_claim(b"persisted-selection-other-generation");
        let wrong_generation = selected.for_generation(other_claim.binding().identity);
        let wrong_history = BuiltinIntent::select_semantic_generation(
            package,
            label,
            selected,
            wrong_generation,
            Some(before),
            after,
        )
        .expect_err("history binding mismatch");
        assert!(
            wrong_history
                .to_string()
                .contains("semantic selection generation")
        );
    }
}

#[cfg(test)]
#[path = "profile_membership_tests.rs"]
mod membership_tests;

#[cfg(test)]
#[path = "profile_source_facts_tests.rs"]
mod source_facts_tests;
