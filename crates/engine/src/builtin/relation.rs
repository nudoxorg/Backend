//! Typed source relations and owner-held lazy source snapshots.

use super::complete_coverage;
use crate::workspace::{TransitionWork, WorkspaceRelationHandle, WorkspaceSnapshot};
pub use backend_compile::{DeclarationKind, SourceDeclaration, SourceLanguage, SourceLocation};
use backend_compile::{SourceExcerpt, SourceExcerptExtent};
use backend_execution::AuthorityVersion;
use backend_replication::ImmutableObjectSchema;
use backend_version::{
    CanonicalRelation, CoverageWitness, Relation, RelationState, StateRoot, WorkspaceRoot,
};
use std::sync::Arc;

/// Authoritative package/source coordinates retained by the product
/// workspace. Product input identities are always rooted in this relation;
/// the echo fixture's numeric relation is kept separate above.
#[derive(Debug)]
pub struct ProductSourceRelation;

/// Canonical project or file metadata retained by the product source relation.
///
/// A project record owns the sorted file-key frontier for O(changed files)
/// reconciliation. Each file is an independent relation value and therefore
/// changes without rewriting the other files or their declaration payloads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductSourceRecord {
    /// One indexed project and the exact set of file rows it selects.
    Project {
        /// Canonical user-facing project coordinate.
        label: String,
        /// Digest over the sorted file identities and content versions.
        source_version: [u8; 32],
        /// Sorted relation keys for every selected source file.
        files: Arc<[[u8; 32]]>,
    },
    /// One independently versioned source file.
    File {
        /// Project key that owns this file.
        project: [u8; 32],
        /// Canonical project-relative path.
        path: String,
        /// Closed language authority.
        language: SourceLanguage,
        /// Content digest of the exact source bytes.
        content_version: [u8; 32],
        /// Identity of the parser, grammar, and extraction contract.
        analysis_version: [u8; 32],
        /// Ordered declarations extracted from this file.
        declarations: Arc<[SourceDeclaration]>,
    },
}

/// Borrowed project frontier exposed without tuple-position coupling.
#[derive(Clone, Copy, Debug)]
pub struct ProductProjectRef<'a> {
    /// Canonical user-facing project coordinate.
    pub label: &'a str,
    /// Digest over the selected file identities and content versions.
    pub source_version: [u8; 32],
    /// Sorted relation keys for the selected files.
    pub files: &'a [[u8; 32]],
}

/// Borrowed source-file fields exposed without cloning declaration storage.
#[derive(Clone, Copy, Debug)]
pub struct ProductFileRef<'a> {
    /// Project relation key that owns the file.
    pub project: [u8; 32],
    /// Canonical project-relative path.
    pub path: &'a str,
    /// Closed language authority.
    pub language: SourceLanguage,
    /// Digest of the exact source bytes.
    pub content_version: [u8; 32],
    /// Parser, grammar, and extraction contract identity.
    pub analysis_version: [u8; 32],
    /// Shared declaration storage.
    pub declarations: &'a Arc<[SourceDeclaration]>,
}

impl ProductSourceRecord {
    /// Maximum admitted coordinate or relative-path length.
    pub const MAX_LABEL_BYTES: usize = 4096;
    /// Maximum source files retained by one project record.
    pub const MAX_PROJECT_FILES: usize = 100_000;
    /// Maximum declarations retained in one source file.
    pub const MAX_FILE_DECLARATIONS: usize = 16_384;

    /// Constructs a checked empty project record for compatibility package
    /// coordinates.
    /// # Errors
    ///
    /// Returns an error when the coordinate is empty or oversized.
    pub fn new(label: impl Into<String>) -> Result<Self, String> {
        Self::project(label, [0; 32], Vec::new())
    }

    /// Constructs a checked project frontier.
    ///
    /// # Errors
    /// Returns an error for an invalid label or unsorted, duplicate, or
    /// oversized file-key frontier.
    pub fn project(
        label: impl Into<String>,
        source_version: [u8; 32],
        files: impl Into<Vec<[u8; 32]>>,
    ) -> Result<Self, String> {
        let label = label.into();
        let files = files.into();
        if label.is_empty() || label.len() > Self::MAX_LABEL_BYTES {
            return Err("project source coordinate is empty or oversized".to_owned());
        }
        if files.len() > Self::MAX_PROJECT_FILES
            || files.windows(2).any(|window| window[0] >= window[1])
        {
            return Err("project file frontier is unordered or oversized".to_owned());
        }
        Ok(Self::Project {
            label,
            source_version,
            files: Arc::from(files.into_boxed_slice()),
        })
    }

    /// Constructs a checked file record.
    ///
    /// # Errors
    /// Returns an error for an invalid path or oversized declaration set.
    pub fn file(
        project: [u8; 32],
        path: impl Into<String>,
        language: SourceLanguage,
        content_version: [u8; 32],
        analysis_version: [u8; 32],
        declarations: impl Into<Arc<[SourceDeclaration]>>,
    ) -> Result<Self, String> {
        let path = path.into();
        let declarations = declarations.into();
        if path.is_empty() || path.len() > Self::MAX_LABEL_BYTES {
            return Err("source file path is empty or oversized".to_owned());
        }
        if declarations.len() > Self::MAX_FILE_DECLARATIONS {
            return Err("source file declaration set is oversized".to_owned());
        }
        Ok(Self::File {
            project,
            path,
            language,
            content_version,
            analysis_version,
            declarations,
        })
    }

    /// Returns the display coordinate for this project or file.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Project { label, .. } => label,
            Self::File { path, .. } => path,
        }
    }

    /// Returns the project frontier fields when this is a project record.
    #[must_use]
    pub fn project_fields(&self) -> Option<ProductProjectRef<'_>> {
        match self {
            Self::Project {
                label,
                source_version,
                files,
            } => Some(ProductProjectRef {
                label,
                source_version: *source_version,
                files,
            }),
            Self::File { .. } => None,
        }
    }

    /// Returns the file fields when this is a file record.
    #[must_use]
    pub fn file_fields(&self) -> Option<ProductFileRef<'_>> {
        match self {
            Self::File {
                project,
                path,
                language,
                content_version,
                analysis_version,
                declarations,
            } => Some(ProductFileRef {
                project: *project,
                path,
                language: *language,
                content_version: *content_version,
                analysis_version: *analysis_version,
                declarations,
            }),
            Self::Project { .. } => None,
        }
    }
}

impl Relation for ProductSourceRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 2;
    type Key = [u8; 32];
    type Value = ProductSourceRecord;

    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(b"PSR7");
        match value {
            ProductSourceRecord::Project {
                label,
                source_version,
                files,
            } => {
                output.push(1);
                push_text(output, label);
                output.extend_from_slice(source_version);
                push_count(output, files.len());
                for file in files.iter() {
                    output.extend_from_slice(file);
                }
            }
            ProductSourceRecord::File {
                project,
                path,
                language,
                content_version,
                analysis_version,
                declarations,
            } => {
                output.push(2);
                output.extend_from_slice(project);
                push_text(output, path);
                output.push(language.wire_tag());
                output.extend_from_slice(content_version);
                output.extend_from_slice(analysis_version);
                push_count(output, declarations.len());
                for declaration in declarations.iter() {
                    output.extend_from_slice(&declaration.line().to_be_bytes());
                    if declaration.location().path() == path {
                        push_text(output, declaration.location().path());
                    } else {
                        // The file relation remains the authority for the
                        // selected path.  Retaining a declaration's typed
                        // location is useful for callers, but an inconsistent
                        // path must not silently alter the file identity.
                        push_text(output, path);
                    }
                    push_text(output, declaration.name());
                    output.push(declaration.kind().wire_tag());
                    push_text(output, declaration.signature());
                    push_text(output, declaration.documentation());
                    match declaration.source_excerpt() {
                        SourceExcerpt::NotCaptured => output.push(0),
                        SourceExcerpt::Captured {
                            text,
                            extent: SourceExcerptExtent::Complete,
                        } => {
                            output.push(1);
                            push_text(output, text);
                        }
                        SourceExcerpt::Captured {
                            text,
                            extent: SourceExcerptExtent::Truncated,
                        } => {
                            output.push(2);
                            push_text(output, text);
                        }
                        SourceExcerpt::NotHydrated => output.push(3),
                        SourceExcerpt::Unconfigured => output.push(4),
                    }
                }
            }
        }
    }
}

impl CanonicalRelation for ProductSourceRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, backend_version::RelationDecodeError> {
        bytes
            .try_into()
            .map_err(|_| backend_version::RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, backend_version::RelationDecodeError> {
        decode_source_record(bytes).map_err(|()| backend_version::RelationDecodeError::Malformed)
    }
}

/// Derives the relation key for a project-relative source file.
#[must_use]
pub fn product_source_file_key(project: [u8; 32], path: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.product-source.file.v1\0");
    hasher.update(&project);
    hasher.update(&(path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    *hasher.finalize().as_bytes()
}

fn push_count(output: &mut Vec<u8>, count: usize) {
    output.extend_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_be_bytes());
}

fn push_text(output: &mut Vec<u8>, value: &str) {
    push_count(output, value.len());
    output.extend_from_slice(value.as_bytes());
}

fn decode_source_record(bytes: &[u8]) -> Result<ProductSourceRecord, ()> {
    let mut reader = SourceReader::new(bytes);
    let format = reader.take(4)?;
    if !matches!(
        format,
        b"PSR2" | b"PSR3" | b"PSR4" | b"PSR5" | b"PSR6" | b"PSR7"
    ) {
        return Err(());
    }
    let record = match reader.byte()? {
        1 => {
            let label = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let source_version = reader.array()?;
            let count = reader.count(ProductSourceRecord::MAX_PROJECT_FILES)?;
            let mut files = Vec::with_capacity(count);
            for _ in 0..count {
                files.push(reader.array()?);
            }
            ProductSourceRecord::project(label, source_version, files).map_err(|_| ())?
        }
        2 => {
            let project = reader.array()?;
            let path = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let language = SourceLanguage::from_wire_tag(reader.byte()?).ok_or(())?;
            let content_version = reader.array()?;
            let analysis_version =
                if matches!(format, b"PSR3" | b"PSR4" | b"PSR5" | b"PSR6" | b"PSR7") {
                    reader.array()?
                } else {
                    [0; 32]
                };
            let count = reader.count(ProductSourceRecord::MAX_FILE_DECLARATIONS)?;
            let mut declarations = Vec::with_capacity(count);
            for _ in 0..count {
                let line = reader.u32()?;
                let declaration_path = if matches!(format, b"PSR4" | b"PSR5" | b"PSR6" | b"PSR7") {
                    reader.text(SourceLocation::MAX_PATH_BYTES)?
                } else {
                    path.clone()
                };
                if declaration_path != path {
                    return Err(());
                }
                let name = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
                let kind = if matches!(format, b"PSR4" | b"PSR5" | b"PSR6" | b"PSR7") {
                    DeclarationKind::from_wire_tag(reader.byte()?).ok_or(())?
                } else {
                    DeclarationKind::from_name(&reader.text(SourceDeclaration::MAX_TEXT_BYTES)?)
                };
                let signature = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
                let documentation = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
                let source_excerpt = if matches!(format, b"PSR5" | b"PSR6" | b"PSR7") {
                    decode_source_excerpt(&mut reader)?
                } else {
                    SourceExcerpt::NotCaptured
                };
                declarations.push(
                    SourceDeclaration::with_location(
                        SourceLocation::new(declaration_path, line).map_err(|_| ())?,
                        name,
                        kind.name(),
                        signature,
                        documentation,
                    )
                    .map_err(|_| ())?
                    .with_source_excerpt(source_excerpt),
                );
            }
            if format == b"PSR6" {
                skip_legacy_semantics(&mut reader)?;
            }
            ProductSourceRecord::file(
                project,
                path,
                language,
                content_version,
                analysis_version,
                declarations,
            )
            .map_err(|_| ())?
        }
        _ => return Err(()),
    };
    reader.finish()?;
    Ok(record)
}

fn skip_legacy_semantics(reader: &mut SourceReader<'_>) -> Result<(), ()> {
    let tag = reader.byte()?;
    if matches!(tag, 0 | 1) {
        return Ok(());
    }
    if !matches!(tag, 2 | 3) {
        return Err(());
    }
    let _: [u8; 32] = reader.array()?;
    let _: [u8; 32] = reader.array()?;
    let _ = reader.u64()?;
    let count = reader.count(backend_compile::MAX_NATIVE_RECORDS)?;
    for _ in 0..count {
        if !matches!(reader.byte()?, 1..=6) {
            return Err(());
        }
        let _ = reader.bytes(backend_compile::MAX_NATIVE_KEY_BYTES)?;
        let _ = reader.bytes(backend_compile::MAX_NATIVE_VALUE_BYTES)?;
    }
    Ok(())
}

fn decode_source_excerpt(reader: &mut SourceReader<'_>) -> Result<SourceExcerpt, ()> {
    let tag = reader.byte()?;
    match tag {
        0 => Ok(SourceExcerpt::NotCaptured),
        1 | 2 => {
            let text = reader.text(SourceExcerpt::MAX_BYTES)?;
            SourceExcerpt::captured(
                &text,
                if tag == 1 {
                    SourceExcerptExtent::Complete
                } else {
                    SourceExcerptExtent::Truncated
                },
            )
            .map_err(|_| ())
        }
        3 => Ok(SourceExcerpt::NotHydrated),
        4 => Ok(SourceExcerpt::Unconfigured),
        _ => Err(()),
    }
}

struct SourceReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> SourceReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ()> {
        let end = self.at.checked_add(length).ok_or(())?;
        let value = self.bytes.get(self.at..end).ok_or(())?;
        self.at = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, ()> {
        self.take(1)?.first().copied().ok_or(())
    }

    fn u32(&mut self) -> Result<u32, ()> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| ())?,
        ))
    }

    fn u64(&mut self) -> Result<u64, ()> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| ())?,
        ))
    }

    fn count(&mut self, maximum: usize) -> Result<usize, ()> {
        let count = self.u32()? as usize;
        (count <= maximum).then_some(count).ok_or(())
    }

    fn array(&mut self) -> Result<[u8; 32], ()> {
        self.take(32)?.try_into().map_err(|_| ())
    }

    fn text(&mut self, maximum: usize) -> Result<String, ()> {
        let length = self.count(maximum)?;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| ())
    }

    fn bytes(&mut self, maximum: usize) -> Result<&'a [u8], ()> {
        let length = self.count(maximum)?;
        self.take(length)
    }

    fn finish(self) -> Result<(), ()> {
        (self.at == self.bytes.len()).then_some(()).ok_or(())
    }
}

/// Exact compact source-change facts retained with a checked workspace head.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProductSourceDeltaFacts {
    /// Number of logical source items changed by the selected transition.
    pub changed_items: u64,
    /// Number of canonical nodes rebuilt by the selected transition.
    pub changed_nodes: u64,
    /// Bytes emitted in the changed canonical frontier.
    pub changed_bytes: u64,
}

impl ProductSourceDeltaFacts {
    pub(crate) fn from_transition(work: TransitionWork) -> Self {
        Self {
            changed_items: work.changed_items(),
            changed_nodes: work.changed_nodes(),
            changed_bytes: work.changed_bytes(),
        }
    }
}

/// Authenticated local-retention facts carried by a checked product source.
///
/// These facts describe the selected source arrangement and closure that the
/// owner can actually retain.  They are read from authenticated root
/// summaries, so placement code does not substitute request cardinalities or
/// scan the complete relation before choosing a route.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProductSourceRetentionFacts {
    /// Rows available from the selected product relation root.
    pub relation_rows: u64,
    /// Height of the selected relation root.
    pub relation_level: u16,
    /// Object descriptors retained by the selected workspace closure.
    pub retained_objects: u64,
}

/// Exact source-lane transition roots selected by an admitted workspace
/// snapshot. The raw bytes stay private; consumers can only ask whether both
/// roots match their already typed prior/current roots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductSourceTransition {
    base: [u8; backend_version::ID_BYTES],
    target: [u8; backend_version::ID_BYTES],
}

impl ProductSourceTransition {
    /// Returns whether this checked transition is adjacent to the supplied
    /// prior and current product relation roots.
    #[must_use]
    pub fn binds(
        self,
        prior: StateRoot<ProductSourceRelation>,
        target: StateRoot<ProductSourceRelation>,
    ) -> bool {
        self.base == *prior.as_bytes() && self.target == *target.as_bytes()
    }
}

/// A checked source relation selected by an owner workspace head.
///
/// The handle keeps the store and authenticated relation root alive while
/// callers perform bounded page reads. It does not copy relation rows.
#[derive(Clone, Debug)]
pub struct ProductSourceSnapshot {
    workspace: WorkspaceRoot,
    transition: ProductSourceTransition,
    relation: WorkspaceRelationHandle<ProductSourceRelation>,
    manifest: Arc<[u8]>,
    authority: [u8; backend_version::ID_BYTES],
    delta: ProductSourceDeltaFacts,
    retention: ProductSourceRetentionFacts,
}

impl ProductSourceSnapshot {
    /// Opens the product source selected by a checked workspace snapshot.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn from_workspace(snapshot: &WorkspaceSnapshot) -> Result<Self, String> {
        snapshot
            .manifest()
            .validate()
            .map_err(|error| error.to_string())?;
        let relation = snapshot
            .relation::<ProductSourceRelation>()
            .map_err(|error| error.to_string())?;
        let relation_root = relation.root_node().map_err(|error| error.to_string())?;
        let (base, target) = snapshot
            .transition_relation_roots::<ProductSourceRelation>()
            .ok_or_else(|| "selected workspace has no product source transition".to_owned())?;
        if target != *relation.root().as_bytes() {
            return Err(
                "product source transition target differs from selected relation".to_owned(),
            );
        }
        Ok(Self {
            workspace: snapshot.root(),
            transition: ProductSourceTransition { base, target },
            relation,
            manifest: Arc::from(snapshot.manifest().encode().into_boxed_slice()),
            authority: *snapshot.manifest().authority(),
            delta: ProductSourceDeltaFacts::from_transition(snapshot.transition_work()),
            retention: ProductSourceRetentionFacts {
                relation_rows: relation_root.row_count(),
                relation_level: relation_root.level(),
                retained_objects: snapshot.closure().manifest().object_count(),
            },
        })
    }

    /// Returns the checked workspace root that selected this source.
    #[must_use]
    pub const fn workspace_root(&self) -> WorkspaceRoot {
        self.workspace
    }

    /// Returns the exact checked source relation transition.
    #[must_use]
    pub const fn transition(&self) -> ProductSourceTransition {
        self.transition
    }

    /// Returns the checked product relation root.
    #[must_use]
    pub fn relation_root(&self) -> StateRoot<ProductSourceRelation> {
        self.relation.root()
    }

    /// Returns the owner-held lazy relation handle for bounded page reads.
    #[must_use]
    pub const fn relation(&self) -> &WorkspaceRelationHandle<ProductSourceRelation> {
        &self.relation
    }

    /// Returns exact source change facts carried by the selected head.
    #[must_use]
    pub const fn delta_facts(&self) -> ProductSourceDeltaFacts {
        self.delta
    }

    /// Returns authenticated local arrangement and closure retention facts.
    #[must_use]
    pub const fn retention_facts(&self) -> ProductSourceRetentionFacts {
        self.retention
    }

    /// Returns the exact checked workspace manifest selected with this source.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest
    }

    /// Returns the authority identity admitted by the selected workspace
    /// manifest.
    #[must_use]
    pub const fn authority_bytes(&self) -> &[u8; backend_version::ID_BYTES] {
        &self.authority
    }

    /// Returns the typed immutable input capability bound to this source.
    #[must_use]
    pub fn input(&self) -> ProductInput {
        ProductInput {
            root: self.relation.root(),
        }
    }
}

/// Typed product input capability derived from an admitted source relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductInput {
    root: StateRoot<ProductSourceRelation>,
}

impl ProductInput {
    /// Creates an input capability from a checked relation state.
    #[must_use]
    pub fn from_checked_relation(relation: &RelationState<ProductSourceRelation>) -> Self {
        Self {
            root: relation.root(),
        }
    }

    /// Returns the checked source relation root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<ProductSourceRelation> {
        self.root
    }
}

/// Immutable object schema used for product relation input transfer.
pub type BuiltinInputSchema = ImmutableObjectSchema;

/// Builds a checked Product-shaped source fixture for compatibility adapters.
pub(super) fn product_source_fixture(
    expanded: bool,
) -> Result<RelationState<ProductSourceRelation>, String> {
    let entries = source_entries(expanded);
    RelationState::from_entries(
        entries,
        CoverageWitness::Partial(backend_version::partial_coverage(1)),
    )
    .map_err(|error| error.to_string())
}

/// Builds a Product-shaped source fixture with complete authority coverage.
pub(super) fn product_source_fixture_with_authority(
    expanded: bool,
    authority: AuthorityVersion,
) -> Result<RelationState<ProductSourceRelation>, String> {
    RelationState::from_entries(source_entries(expanded), complete_coverage(authority)?)
        .map_err(|error| error.to_string())
}

fn source_entries(expanded: bool) -> Vec<([u8; 32], ProductSourceRecord)> {
    let mut entries = vec![(
        backend_library::package_key("backend-builtin").to_bytes(),
        ProductSourceRecord::Project {
            label: "backend-builtin".to_owned(),
            source_version: [0; 32],
            files: Arc::from([]),
        },
    )];
    if expanded {
        entries.push((
            backend_library::package_key("backend-extra").to_bytes(),
            ProductSourceRecord::Project {
                label: "backend-extra".to_owned(),
                source_version: [0; 32],
                files: Arc::from([]),
            },
        ));
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::{
        DeclarationKind, ProductSourceRecord, ProductSourceRelation, Relation, SourceDeclaration,
        SourceExcerpt, SourceExcerptExtent, SourceLocation,
    };
    use backend_version::CanonicalRelation;
    use std::sync::Arc;

    #[test]
    fn source_relation_round_trips_typed_declaration_metadata() {
        let declaration = SourceDeclaration::at_path(
            "src/lib.rs",
            "answer",
            "function",
            7,
            "fn answer() -> u32",
            "the answer",
        )
        .expect("declaration")
        .with_source_excerpt(
            SourceExcerpt::captured("fn answer() -> u32 { 42 }", SourceExcerptExtent::Complete)
                .expect("excerpt"),
        );
        let record = ProductSourceRecord::file(
            [3; 32],
            "src/lib.rs",
            super::SourceLanguage::Rust,
            [4; 32],
            [5; 32],
            Arc::from([declaration]),
        )
        .expect("file");
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&record, &mut encoded);
        assert_eq!(&encoded[..4], b"PSR7");
        let decoded = ProductSourceRelation::decode_value(&encoded).expect("decode");
        let fields = decoded.file_fields().expect("file fields");
        let declaration = &fields.declarations[0];
        assert_eq!(declaration.kind(), DeclarationKind::Function);
        assert_eq!(declaration.location().path(), "src/lib.rs");
        assert_eq!(declaration.location().start_line(), 7);
        assert_eq!(
            SourceLocation::new("src/lib.rs", 7).expect("location"),
            *declaration.location()
        );
        assert_eq!(
            declaration.source_excerpt().text(),
            Some("fn answer() -> u32 { 42 }")
        );
        assert_eq!(
            declaration.source_excerpt().extent(),
            Some(SourceExcerptExtent::Complete)
        );
    }
}
