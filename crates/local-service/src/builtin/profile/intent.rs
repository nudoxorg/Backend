//! Canonical bytes for one builtin package intent.
//!
//! `BPI3` and `BPI4` are the persisted operation. Restart admission re-encodes
//! the intent and refuses any byte that is not identical, including the
//! selected compiler generation and its before/after semantic rows.

use backend_engine::builtin::{
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord,
    ProductSemanticPublicationRelation,
};
use backend_engine::{CanonicalRelation, Relation, WorkspaceRelationHandle};

use super::{
    BuiltinIntent, BuiltinIntentOperation, BuiltinModelError, BuiltinPackageRecord,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSemanticSelectionIntent,
    BuiltinSourceChange, ProductRelation,
};

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

    pub(in crate::builtin) fn remove_project_with_semantics(
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

    pub(in crate::builtin) fn index_with_semantics(
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

    pub(in crate::builtin) fn select_semantic_generation(
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
            semantic_selection,
        })
    }

    pub(in crate::builtin) fn encode(&self) -> Vec<u8> {
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

    pub(super) fn canonical_bytes(&self) -> Vec<u8> {
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

    pub(in crate::builtin) fn record(&self) -> Result<BuiltinPackageRecord, BuiltinModelError> {
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

    pub(super) fn admit_semantic_selection_against(
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
