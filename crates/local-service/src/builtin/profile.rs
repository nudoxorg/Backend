//! Compiled profile identities, model, validator, and dispatcher.
//!
//! This module is the profile composition boundary. It owns the registered
//! relation recipe and its checked authority/semantic contracts; process and
//! transport state stay in the parent façade and the replication module.

use super::{
    AUTHORITY_VALUE, Arc, AuthorityVersionSchema, Blake3AuthorityVerifier, Budget, Commit,
    CommitProvenance, CompleteSemanticCoverage, CompositeAdmissionValidator, DependencyManifest,
    DispatchError, Dispatcher, ECHO_AUTHORITY_SECRET, LazyClosureUpdate, ObjectClosure, ObjectKey,
    ObjectVersion, OutputVersion, PreparedTransition, RelationAdmissionRegistry,
    RelationTransition, RemoteAuthorityPolicy, ResourceVector, Scheduler, Schema,
    SemanticCoverageAdmissionError, SemanticCoverageBinding, TransactionId, TypedObject,
    UntrustedSemanticCoverageClaim, WorkspaceClosure, WorkspaceDelta, WorkspaceManifest,
    WorkspaceModel, WorkspaceSnapshot, admit_manifest, fmt, transition_closure_lazy,
    workspace_manifest_from_root,
};
#[cfg(test)]
use backend_engine::builtin::SemanticPublicationClaim;
use backend_engine::builtin::{
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord,
    ProductSemanticPublicationRelation,
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

fn prepare_source_update(
    relation: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    changes: &[BuiltinSourceChange],
) -> Result<LazyPreparedUpdate<BuiltinWorkspaceRelation>, BuiltinModelError> {
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
    semantic_selection: Option<BuiltinSemanticSelectionIntent>,
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
        Self::remove_project(package, label, &[])
    }

    /// Creates an intent that removes a project and every file selected by
    /// its last admitted frontier.
    pub(super) fn remove_project(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        files: &[[u8; 32]],
    ) -> Result<Self, BuiltinModelError> {
        let label = label.into();
        let mut changes = Vec::with_capacity(files.len().saturating_add(1));
        changes.push(BuiltinSourceChange {
            key: package.to_bytes(),
            after: None,
        });
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
            Vec::new(),
            None,
        )
    }

    pub(super) fn remove_project_with_semantics(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        files: &[[u8; 32]],
        semantic_changes: Vec<BuiltinSemanticChange>,
    ) -> Result<Self, BuiltinModelError> {
        let label = label.into();
        let mut changes = Vec::with_capacity(files.len().saturating_add(1));
        changes.push(BuiltinSourceChange {
            key: package.to_bytes(),
            after: None,
        });
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
    ) -> Result<Self, BuiltinModelError> {
        if !matches!(encoding_version, 3 | Self::VERSION)
            || (encoding_version < Self::VERSION
                && (matches!(operation, BuiltinIntentOperation::SelectSemanticGeneration)
                    || semantic_selection.is_some()))
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
        if changes.len() > BuiltinPackageRecord::MAX_PROJECT_FILES.saturating_add(1)
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
        if (changes.is_empty() && semantic_changes.is_empty())
            || semantic_changes.len() > compiler_application::MAX_MANIFEST_ENTRIES
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
            semantic_selection,
        })
    }

    pub(super) fn encode(&self) -> Vec<u8> {
        self.encode_canonical()
    }

    fn encode_canonical(&self) -> Vec<u8> {
        let label = self.label.as_bytes();
        let mut bytes = Vec::with_capacity(46 + label.len());
        bytes.extend_from_slice(if self.encoding_version == 3 {
            b"BPI3"
        } else {
            b"BPI4"
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
        decoder.finish()?;
        Self::new_with_version(
            encoding_version,
            operation,
            package,
            label,
            changes,
            semantic_changes,
            semantic_selection,
        )
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
            backend_semantic::vocabulary::LanguageProfile::Rust(backend_semantic::vocabulary::RustEdition::Rust2024)
        }
        backend_semantic::vocabulary::PackageType::Npm => backend_semantic::vocabulary::LanguageProfile::TypeScript(
            backend_semantic::vocabulary::TypeScriptSource::TypeScript,
        ),
        backend_semantic::vocabulary::PackageType::Pypi => backend_semantic::vocabulary::LanguageProfile::Python(
            backend_semantic::vocabulary::PythonVersion::Python314,
        ),
        backend_semantic::vocabulary::PackageType::Golang => {
            backend_semantic::vocabulary::LanguageProfile::Go(backend_semantic::vocabulary::GoVersion::Go125)
        }
        backend_semantic::vocabulary::PackageType::Maven => {
            backend_semantic::vocabulary::LanguageProfile::Java(backend_semantic::vocabulary::JavaRelease::Java25)
        }
        backend_semantic::vocabulary::PackageType::Nuget => backend_semantic::vocabulary::LanguageProfile::CSharp(
            backend_semantic::vocabulary::CSharpVersion::CSharp14,
        ),
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
                (Some(b"BPI3"), 3) | (Some(b"BPI4"), 4)
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
        if count > BuiltinPackageRecord::MAX_PROJECT_FILES.saturating_add(1) {
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
        if count > compiler_application::MAX_MANIFEST_ENTRIES {
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
        let intent = intent.clone();
        let relation = base
            .relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
        let semantic = base
            .relation::<BuiltinSemanticRelation>()
            .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
        intent.admit_semantic_selection_against(&semantic)?;
        let update = prepare_source_update(&relation, intent.changes())?;
        let semantic_update = prepare_semantic_update(&semantic, intent.semantic_changes())?;
        let source_changed = update.delta().changes().next().is_some();
        let semantic_changed = semantic_update.delta().changes().next().is_some();
        let changed_items = update
            .delta()
            .changes()
            .count()
            .saturating_add(semantic_update.delta().changes().count());
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
        let semantic_target_root = semantic_update.target().root();
        let request = self.request_id(&intent);
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
                intent: &intent,
            },
        )?;
        let registry = RelationAdmissionRegistry::new()
            .with_relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?
            .with_relation::<BuiltinSemanticRelation>()
            .map_err(|error| BuiltinModelError(format!("register semantic relation: {error:?}")))?;
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
        transition
            .replace_object_family([intent_object], &registry)
            .map(|transition| transition.with_work(work))
            .map_err(|error| BuiltinModelError(format!("retain current intent: {error}")))
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

fn admit_persisted_transition(
    persisted: &backend_engine::PersistedTransition,
    store: &backend_engine::FileStore,
) -> Result<PreparedTransition, BuiltinModelError> {
    let persisted_intent = admit_persisted_intent(persisted.closure_manifest().objects())?;
    let untrusted_manifest = WorkspaceManifest::decode_untrusted(persisted.manifest_bytes())
        .map_err(|error| BuiltinModelError(format!("decode persisted manifest: {error}")))?;

    // The rest of restart admission reopens both exact relation roots and
    // proves that this typed intent alone reproduces the authenticated delta.
    admit_persisted_relations(persisted, store, persisted_intent, untrusted_manifest)
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
    if persisted_intent.encode().as_slice() != intent_object.bytes() {
        return Err(BuiltinModelError(
            "persisted builtin intent is not canonically encoded".to_owned(),
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
    let update = prepare_source_update(&base_tree, persisted_intent.changes())?;
    let semantic_update =
        prepare_semantic_update(&base_semantic, persisted_intent.semantic_changes())?;
    let source_changed = update.delta().changes().next().is_some();
    let semantic_changed = semantic_update.delta().changes().next().is_some();
    let changed_items = update
        .delta()
        .changes()
        .count()
        .saturating_add(semantic_update.delta().changes().count());
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
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?
        .with_relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("register semantic relation: {error:?}")))?;
    // Reopen the exact authenticated closure selected by the physical HEAD.
    // The compact transition envelope intentionally contains only recovery
    // pointers, so rebuilding a new manifest from that subset would produce a
    // different closure identity. The durable manifest remains content
    // addressed and is admitted through the store's relation registry here.
    let selected = store
        .head()
        .map_err(|error| BuiltinModelError(format!("read selected workspace head: {error:?}")))?
        .ok_or_else(|| BuiltinModelError("selected workspace head disappeared".to_owned()))?;
    let persisted_objects = store
        .read_closure(selected.descriptor().closure())
        .map_err(|error| {
            BuiltinModelError(format!("read selected workspace closure: {error:?}"))
        })?;
    let closure = WorkspaceClosure::from_checked_transition_root_only_with_registry(
        &manifest,
        &checked_delta,
        Some(&checked_commit),
        persisted_objects,
        &registry,
    )
    .map_err(|error| BuiltinModelError(format!("admit persisted closure: {error:?}")))?;
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
pub(super) struct BuiltinOutputValidator;

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
pub(super) struct BuiltinSemanticAuthority {
    profile: Arc<ProfileDescriptor>,
}
pub(super) type BuiltinValidator =
    CompositeAdmissionValidator<BuiltinOutputValidator, BuiltinSemanticAuthority>;
pub(super) type BuiltinAuthorityVerifier = Blake3AuthorityVerifier;

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
    use compiler_publication::{
        binding::{COMPILATION_BINDING_BYTES, CompilationBindingView},
        manifest::{CompilationManifestFacts, CompilationManifestFormat},
    };
    use heart_hydration::VerifiedGenerationFacts;
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
        let coordinate =
            backend_semantic::vocabulary::PackageUrl::parse(label.to_owned()).expect("selection coordinate");
        let selected = ProductSemanticPublicationKey::new(
            package_reference,
            coordinate,
            backend_semantic::vocabulary::LanguageProfile::Rust(backend_semantic::vocabulary::RustEdition::Rust2024),
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
