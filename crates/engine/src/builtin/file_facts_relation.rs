//! Complete, independently paged structural source facts.
//!
//! `ProductSourceRelation` is deliberately a compact browsing index. A single
//! source row has a hard canonical node limit, so it may shed excerpts or
//! declaration tails while stating that loss in `DeclarationRetention`.
//! This relation keeps the complete extraction beside that compact row. Its
//! manifest binds the file key, exact source and analysis identities, count,
//! and ordered page tree. Declaration pages contain no whole-file version;
//! unchanged pages can therefore keep their keys when another part of the
//! file changes.

use super::relation::{
    Container, DeclarationKind, ProductFileRef, ProductSourceRecord, ProductSourceRelation,
    SourceDeclaration, SourceLanguage, SourceLocation, product_source_file_key,
};
use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_compile::{
    DeclarationFacts, Deprecation, Fact, MAX_FACT_TEXT_BYTES, Obligation, SourceExcerpt,
    SourceExcerptExtent,
};
use backend_store::TypedObject;
use backend_version::{
    CanonicalRelation, ContentId, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity,
    SourceFactDomain, StateRoot,
};
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;

const MAGIC: &[u8; 4] = b"PSF1";
const MANIFEST_TAG: u8 = 1;
const DIRECTORY_TAG: u8 = 2;
const DECLARATION_PAGE_TAG: u8 = 3;
const MIN_CONTENT_CUT: usize = 16;
const MAX_DECLARATIONS_PER_PAGE: usize = 256;
const MAX_PAGES_PER_DIRECTORY: usize = 256;
const FACTS_PAGE_HEADROOM: usize = 1024;
const ROOT_KEY: [u8; 32] = [0x46; 32];

/// Versioned relation containing one complete declaration-facts manifest and
/// its bounded immutable pages for each indexed source file.
#[derive(Debug)]
pub struct ProductSourceFileFactsRelation;

/// Closure pointer schema for the auxiliary complete source-facts relation.
pub struct ProductSourceFileFactsRootSchema;

impl Schema for ProductSourceFileFactsRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 7;
    const VERSION: u8 = 1;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Creates the canonical closure object that points at one checked source-facts root.
#[must_use]
pub fn product_source_file_facts_root_object(
    root: StateRoot<ProductSourceFileFactsRelation>,
) -> TypedObject {
    let key = ObjectKey::<ProductSourceFileFactsRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}

/// Opens source facts selected by this exact immutable workspace closure.
///
/// A missing pointer is the legacy state and carries no complete source facts.
pub fn product_source_file_facts_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceRelationHandle<ProductSourceFileFactsRelation>>, WorkspaceRelationError>
{
    let key = ObjectKey::<ProductSourceFileFactsRootSchema>::from_value(&ROOT_KEY);
    snapshot.auxiliary_relation::<ProductSourceFileFactsRelation>(
        SchemaIdentity::new(
            ProductSourceFileFactsRootSchema::DOMAIN,
            ProductSourceFileFactsRootSchema::TYPE,
            ProductSourceFileFactsRootSchema::VERSION,
        ),
        *key.as_bytes(),
    )
}

/// Exact source identity and complete facts layout for one source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductSourceFileFactsManifest {
    project: [u8; 32],
    path: String,
    content_version: [u8; 32],
    analysis_version: [u8; 32],
    source_identity: ContentId<SourceFactDomain>,
    status: ProductSourceFileFactsStatus,
}

/// Stored layout. Admission turns only these two complete layouts into the
/// closed [`ProductSourceFileFactsAdmission`] type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductSourceFileFactsStatus {
    /// All extracted declarations are retained in this manifest row.
    InlineComplete(Arc<[SourceDeclaration]>),
    /// The exact declaration count and ordered directory pages for a larger
    /// fact set. The page tree is not trusted until admitted against its file
    /// row and every referenced page.
    Paged {
        /// Total extracted declarations, including declarations with no
        /// optional excerpt or compiler-only facts.
        declaration_count: u32,
        /// Ordered page-directory roots.
        directories: Arc<[ProductSourceFactsDirectoryRef]>,
    },
}

/// One ordered directory reference in a file manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductSourceFactsDirectoryRef {
    /// Content key of the directory row.
    pub key: [u8; 32],
    /// Absolute one-based start line of the first declaration page in this
    /// directory. Line changes before the directory update this small
    /// manifest reference while leaving declaration-page payloads stable.
    pub base_line: u32,
    /// Complete declaration count reachable through the directory.
    pub declaration_count: u32,
    /// Number of declaration-page references in the directory.
    pub page_count: u16,
    /// Stable semantic boundary key of the first declaration.
    pub first_boundary: [u8; 32],
    /// Stable semantic boundary key of the last declaration.
    pub last_boundary: [u8; 32],
}

/// One ordered declaration-page reference in a directory row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductSourceFactsPageRef {
    /// Content key of the declaration page.
    pub key: [u8; 32],
    /// Page start line relative to its containing directory's base line.
    pub line_delta: u32,
    /// Number of declarations in the page.
    pub declaration_count: u16,
    /// Stable semantic boundary key of the first declaration.
    pub first_boundary: [u8; 32],
    /// Stable semantic boundary key of the last declaration.
    pub last_boundary: [u8; 32],
}

/// One canonical row of the file-facts relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductSourceFileFactsRecord {
    /// File identity, current source/version binding, count, and page roots.
    Manifest(ProductSourceFileFactsManifest),
    /// Bounded ordered references to declaration pages for one file.
    Directory(ProductSourceFactsDirectory),
    /// One bounded run of complete declaration facts in source order.
    Page(ProductSourceFactsPage),
}

/// Owned lazy lookup retained by an admitted paged file-facts result.
///
/// The closure holds a clone of the owner-selected relation handle. It loads
/// one directory or declaration row at a time when the caller visits pages.
pub type ProductSourceFileFactsLookup =
    Box<dyn FnMut(&[u8; 32]) -> Result<Option<ProductSourceFileFactsRecord>, String>>;

/// Directory row for at most 256 declaration pages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductSourceFactsDirectory {
    file_key: [u8; 32],
    pages: Arc<[ProductSourceFactsPageRef]>,
}

/// A declaration page with source-relative locations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductSourceFactsPage {
    file_key: [u8; 32],
    project: [u8; 32],
    path: String,
    language: SourceLanguage,
    first_boundary: [u8; 32],
    last_boundary: [u8; 32],
    declarations: Arc<[SourceDeclaration]>,
    /// Enclosing container start lines relative to the page base. Other
    /// container kinds carry `None`. The source lines on `declarations` are
    /// one-based offsets from the page base.
    container_line_deltas: Arc<[Option<i64>]>,
}

impl ProductSourceFactsPage {
    /// Stable relation owner for this page.
    #[must_use]
    pub const fn file_key(&self) -> [u8; 32] {
        self.file_key
    }

    /// Number of complete declarations in this bounded page.
    #[must_use]
    pub fn declarations(&self) -> &[SourceDeclaration] {
        &self.declarations
    }
}

/// All relation rows produced for one complete source-file analysis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductSourceFileFactsUpdate {
    manifest_key: [u8; 32],
    manifest: ProductSourceFileFactsManifest,
    pages: Box<[([u8; 32], ProductSourceFileFactsRecord)]>,
    encoded_bytes: usize,
}

impl ProductSourceFileFactsManifest {
    /// Exact file identity whose extracted facts this row binds.
    #[must_use]
    pub fn file_key(&self) -> [u8; 32] {
        product_source_file_key(self.project, &self.path)
    }

    /// Project key owning the source file.
    #[must_use]
    pub const fn project(&self) -> [u8; 32] {
        self.project
    }

    /// Canonical project-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Digest of the exact source bytes analyzed.
    #[must_use]
    pub const fn content_version(&self) -> [u8; 32] {
        self.content_version
    }

    /// Parser, grammar, and extraction-contract identity.
    #[must_use]
    pub const fn analysis_version(&self) -> [u8; 32] {
        self.analysis_version
    }

    /// Semantic compiler identity for the exact same source bytes.
    #[must_use]
    pub const fn source_identity(&self) -> ContentId<SourceFactDomain> {
        self.source_identity
    }

    /// Tests only the exact source identity binding. This does not admit the
    /// directory/page tree; complete admission additionally validates its rows.
    #[must_use]
    pub fn matches_file_identity(&self, file: ProductFileRef<'_>) -> bool {
        self.project == file.project
            && self.path == file.path
            && self.content_version == file.content_version
            && self.analysis_version == file.analysis_version
            && Some(self.source_identity) == file.source_identity
    }

    /// Stored declaration layout.
    #[must_use]
    pub const fn status(&self) -> &ProductSourceFileFactsStatus {
        &self.status
    }
}

impl ProductSourceFileFactsRecord {
    /// Maximum canonical row body admitted by the shared relation tree.
    pub const ROW_VALUE_CAPACITY: usize = ProductSourceRecord::ROW_VALUE_CAPACITY;

    /// Returns the record's file owner when it is a directory or page.
    #[must_use]
    pub fn file_key(&self) -> Option<[u8; 32]> {
        match self {
            Self::Manifest(manifest) => Some(manifest.file_key()),
            Self::Directory(directory) => Some(directory.file_key),
            Self::Page(page) => Some(page.file_key),
        }
    }
}

impl ProductSourceFileFactsUpdate {
    /// Key of the exact file manifest.
    #[must_use]
    pub const fn manifest_key(&self) -> [u8; 32] {
        self.manifest_key
    }

    /// Complete source/version manifest row.
    #[must_use]
    pub const fn manifest(&self) -> &ProductSourceFileFactsManifest {
        &self.manifest
    }

    /// Directory and declaration-page rows in deterministic key order.
    #[must_use]
    pub fn pages(&self) -> &[([u8; 32], ProductSourceFileFactsRecord)] {
        &self.pages
    }

    /// Total encoded bytes of the manifest and all page rows.
    #[must_use]
    pub const fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    /// Total extracted declaration count.
    #[must_use]
    pub fn declaration_count(&self) -> usize {
        match &self.manifest.status {
            ProductSourceFileFactsStatus::InlineComplete(declarations) => declarations.len(),
            ProductSourceFileFactsStatus::Paged {
                declaration_count, ..
            } => usize::try_from(*declaration_count).unwrap_or(usize::MAX),
        }
    }
}

/// Exact borrowed page view. Locations in `declarations` are relative to
/// `base_line`; an enclosing container's true line is `base_line` plus its
/// corresponding signed delta. A visitor can retain neither the page nor its
/// strings after the callback returns without choosing to clone them.
#[derive(Clone, Copy, Debug)]
pub struct ProductSourceFactsPageView<'a> {
    /// Absolute one-based line at which this page starts.
    base_line: u32,
    declarations: &'a [SourceDeclaration],
    container_line_deltas: &'a [Option<i64>],
}

impl<'a> ProductSourceFactsPageView<'a> {
    /// Number of declarations in this page.
    #[must_use]
    pub fn len(self) -> usize {
        self.declarations.len()
    }

    /// Whether this page has no declarations.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.declarations.is_empty()
    }

    /// Returns one declaration with its absolute source and container lines.
    #[must_use]
    pub fn declaration(self, index: usize) -> Option<ProductSourceFactsDeclarationRef<'a>> {
        let declaration = self.declarations.get(index)?;
        let line = self.declaration_line(index)?;
        let container_start_line = self.container_start_line(index);
        Some(ProductSourceFactsDeclarationRef {
            declaration,
            line,
            container_start_line,
        })
    }

    /// Returns the absolute one-based source line for one declaration.
    #[must_use]
    pub fn declaration_line(self, index: usize) -> Option<u32> {
        self.base_line
            .checked_add(self.declarations.get(index)?.line().checked_sub(1)?)
    }

    /// Returns the absolute enclosing-container line, when that declaration
    /// has an enclosing container.
    #[must_use]
    pub fn container_start_line(self, index: usize) -> Option<u32> {
        let delta = self.container_line_deltas.get(index)?.as_ref()?;
        let absolute = i64::from(self.base_line).checked_add(*delta)?;
        u32::try_from(absolute).ok().filter(|line| *line != 0)
    }
}

/// One borrowed full declaration with source-relative paging resolved to the
/// exact current absolute source lines.
#[derive(Clone, Copy, Debug)]
pub struct ProductSourceFactsDeclarationRef<'a> {
    declaration: &'a SourceDeclaration,
    line: u32,
    container_start_line: Option<u32>,
}

impl ProductSourceFactsDeclarationRef<'_> {
    /// Absolute one-based source line.
    #[must_use]
    pub const fn line(self) -> u32 {
        self.line
    }

    /// Absolute one-based enclosing-container line, when present.
    #[must_use]
    pub const fn container_start_line(self) -> Option<u32> {
        self.container_start_line
    }

    /// Borrowed complete structural and compiler-observed facts.
    #[must_use]
    pub fn source_declaration(&self) -> &SourceDeclaration {
        self.declaration
    }

    /// Reconstructs an owned declaration with absolute current line numbers.
    ///
    /// # Errors
    /// Returns an error only if an admitted line cannot form a valid source
    /// location or enclosing container.
    pub fn to_owned(self) -> Result<SourceDeclaration, String> {
        let container = match self.declaration.container() {
            Container::Module => Container::Module,
            Container::Attached { type_name } => Container::attached(type_name),
            Container::Enclosing { name, .. } => Container::enclosing(
                name,
                NonZeroU32::new(
                    self.container_start_line
                        .ok_or_else(|| "admitted container line is missing".to_owned())?,
                )
                .ok_or_else(|| "admitted container line is zero".to_owned())?,
            ),
        };
        Ok(SourceDeclaration::with_location(
            SourceLocation::new(self.declaration.location().path(), self.line)?,
            self.declaration.name(),
            self.declaration.kind(),
            self.declaration.signature(),
            self.declaration.documentation(),
        )
        .map_err(|error| format!("admitted source declaration is invalid: {error}"))?
        .with_source_excerpt(self.declaration.source_excerpt().clone())
        .with_container(container)
        .with_facts(self.declaration.facts().clone()))
    }
}

/// Complete inline facts admitted against the matching compact source row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductSourceFileFactsInline {
    file_key: [u8; 32],
    declarations: Arc<[SourceDeclaration]>,
}

impl ProductSourceFileFactsInline {
    /// Admitted source-file key.
    #[must_use]
    pub const fn file_key(&self) -> [u8; 32] {
        self.file_key
    }

    /// Complete declarations without copying their shared storage.
    #[must_use]
    pub fn declarations(&self) -> &[SourceDeclaration] {
        &self.declarations
    }
}

/// Bounded, already-verified fact pages for one exact source row.
pub struct ProductSourceFileFactsPaged<Lookup> {
    file_key: [u8; 32],
    project: [u8; 32],
    path: String,
    language: SourceLanguage,
    declaration_count: u32,
    directories: Arc<[ProductSourceFactsDirectoryRef]>,
    lookup: Lookup,
}

impl<Lookup> ProductSourceFileFactsPaged<Lookup>
where
    Lookup: FnMut(&[u8; 32]) -> Result<Option<ProductSourceFileFactsRecord>, String>,
{
    /// Admitted source-file key.
    #[must_use]
    pub const fn file_key(&self) -> [u8; 32] {
        self.file_key
    }

    /// Exact complete declaration count verified during admission.
    #[must_use]
    pub const fn declaration_count(&self) -> u32 {
        self.declaration_count
    }

    /// Visits verified pages in source order, holding only one decoded page
    /// at a time. Every lookup is checked again against its owner and content
    /// key before the callback sees it.
    pub fn visit_pages(
        &mut self,
        mut visit: impl FnMut(ProductSourceFactsPageView<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        for directory_ref in self.directories.iter() {
            let directory = lookup_directory(&mut self.lookup, self.file_key, directory_ref)?;
            let mut previous_line = None;
            for page_ref in directory.pages.iter() {
                let page_base = directory_ref
                    .base_line
                    .checked_add(page_ref.line_delta)
                    .ok_or_else(|| "facts page base line exceeds u32".to_owned())?;
                if previous_line.is_some_and(|previous| previous > page_base) {
                    return Err("facts page order moves backwards in the source".to_owned());
                }
                let page = lookup_page(
                    &mut self.lookup,
                    self.file_key,
                    page_ref,
                    &directory_ref.key,
                )?;
                validate_page_identity(&page, self.project, &self.path, self.language, page_base)?;
                let last_line = page_last_absolute_line(&page, page_base)?;
                if previous_line.is_some_and(|previous| previous > page_base) {
                    return Err("facts page declarations are not ordered".to_owned());
                }
                previous_line = Some(last_line);
                visit(ProductSourceFactsPageView {
                    base_line: page_base,
                    declarations: &page.declarations,
                    container_line_deltas: &page.container_line_deltas,
                })?;
            }
        }
        Ok(())
    }
}

/// Closed admission result: checked unavailable extraction, complete inline,
/// or complete facts verified through their page tree.
pub enum ProductSourceFileFactsAdmission<Lookup> {
    /// The selected source could not be extracted, for this exact typed reason.
    /// This carries no complete facts or compiler source authority.
    Unavailable(super::relation::SourceUnavailableReason),
    /// Complete inline facts verified against a source file row.
    InlineComplete(ProductSourceFileFactsInline),
    /// Complete bounded pages verified against a source file row and every
    /// directory/page owner, count, boundary, and digest.
    PagedVerified(ProductSourceFileFactsPaged<Lookup>),
}

/// Builds a complete versioned facts relation update from the exact analysis
/// result before any compact `ProductSourceRecord` retention policy is
/// applied.
///
/// Declaration rows use stable content cuts and relative source spans. The
/// version, source identity, full count, and ordered directory references
/// live only in the file manifest. No declaration or source prose is dropped
/// on success.
///
/// # Errors
/// Returns an error for invalid identity, unordered or path-mismatched
/// declarations, a page that cannot fit one canonical row, or an
/// unrepresentable page tree.
pub fn build_product_source_file_facts(
    project: [u8; 32],
    path: impl Into<String>,
    language: SourceLanguage,
    content_version: [u8; 32],
    analysis_version: [u8; 32],
    source_identity: ContentId<SourceFactDomain>,
    declarations: &[SourceDeclaration],
) -> Result<ProductSourceFileFactsUpdate, String> {
    let path = path.into();
    validate_file_facts_input(&path, declarations)?;
    let file_key = product_source_file_key(project, &path);
    let inline_estimate = declarations
        .iter()
        .fold(96usize.saturating_add(path.len()), |total, declaration| {
            total.saturating_add(estimate_declaration_bytes(declaration, &path))
        });
    if inline_estimate <= ProductSourceFileFactsRecord::ROW_VALUE_CAPACITY {
        let inline_manifest = ProductSourceFileFactsManifest {
            project,
            path: path.clone(),
            content_version,
            analysis_version,
            source_identity,
            status: ProductSourceFileFactsStatus::InlineComplete(Arc::from(declarations)),
        };
        let encoded_manifest = encode_manifest(&inline_manifest);
        if encoded_manifest.len() <= ProductSourceFileFactsRecord::ROW_VALUE_CAPACITY {
            return finish_update(
                file_key,
                inline_manifest,
                Vec::new(),
                encoded_manifest.len(),
            );
        }
    }

    if declarations.is_empty() {
        return Err("empty source facts did not fit the file manifest".to_owned());
    }
    let boundaries = declarations
        .iter()
        .map(declaration_boundary_key)
        .collect::<Result<Vec<_>, _>>()?;
    let page_rows = build_declaration_pages(
        file_key,
        project,
        &path,
        language,
        declarations,
        &boundaries,
    )?;
    let mut directory_rows = Vec::new();
    let mut directory_refs = Vec::new();
    for group in page_rows.chunks(MAX_PAGES_PER_DIRECTORY) {
        let first = group
            .first()
            .ok_or_else(|| "empty facts directory group".to_owned())?;
        let base_line = first.base_line;
        let mut refs = Vec::with_capacity(group.len());
        let mut count = 0usize;
        for page in group {
            let line_delta = page
                .base_line
                .checked_sub(base_line)
                .ok_or_else(|| "facts page line order moved backwards".to_owned())?;
            let declaration_count = u16::try_from(page.record.declarations().len())
                .map_err(|_| "facts page declaration count exceeds u16".to_owned())?;
            count = count
                .checked_add(usize::from(declaration_count))
                .ok_or_else(|| "facts directory declaration count overflowed".to_owned())?;
            refs.push(ProductSourceFactsPageRef {
                key: page.key,
                line_delta,
                declaration_count,
                first_boundary: page.first_boundary,
                last_boundary: page.last_boundary,
            });
        }
        let directory = ProductSourceFactsDirectory {
            file_key,
            pages: Arc::from(refs.into_boxed_slice()),
        };
        let directory_record = ProductSourceFileFactsRecord::Directory(directory);
        let encoded = encode_record(&directory_record);
        if encoded.len() > ProductSourceFileFactsRecord::ROW_VALUE_CAPACITY {
            return Err("facts directory exceeds canonical row capacity".to_owned());
        }
        let directory_key = content_key_bytes(&encoded);
        directory_refs.push(ProductSourceFactsDirectoryRef {
            key: directory_key,
            base_line,
            declaration_count: u32::try_from(count)
                .map_err(|_| "facts directory declaration count exceeds u32".to_owned())?,
            page_count: u16::try_from(group.len())
                .map_err(|_| "facts directory page count exceeds u16".to_owned())?,
            first_boundary: first.first_boundary,
            last_boundary: group
                .last()
                .ok_or_else(|| "empty facts directory group".to_owned())?
                .last_boundary,
        });
        directory_rows.push((directory_key, directory_record, encoded.len()));
    }

    let declaration_count = u32::try_from(declarations.len())
        .map_err(|_| "facts declaration count exceeds u32".to_owned())?;
    let manifest = ProductSourceFileFactsManifest {
        project,
        path,
        content_version,
        analysis_version,
        source_identity,
        status: ProductSourceFileFactsStatus::Paged {
            declaration_count,
            directories: Arc::from(directory_refs.into_boxed_slice()),
        },
    };
    let manifest_bytes = encode_manifest(&manifest);
    if manifest_bytes.len() > ProductSourceFileFactsRecord::ROW_VALUE_CAPACITY {
        return Err("facts manifest exceeds canonical row capacity".to_owned());
    }
    let mut rows = Vec::with_capacity(page_rows.len() + directory_rows.len());
    let mut unique = BTreeMap::new();
    for (key, record, encoded_len) in page_rows
        .into_iter()
        .map(|page| {
            (
                page.key,
                ProductSourceFileFactsRecord::Page(page.record),
                page.encoded_bytes,
            )
        })
        .chain(directory_rows)
    {
        if let Some((prior, _)) = unique.get(&key) {
            if prior != &record {
                return Err("facts page content key collision".to_owned());
            }
        } else {
            unique.insert(key, (record, encoded_len));
        }
    }
    let mut encoded_bytes = manifest_bytes.len();
    for (_, (_, encoded_len)) in &unique {
        encoded_bytes = encoded_bytes
            .checked_add(*encoded_len)
            .ok_or_else(|| "source facts encoded byte count overflowed".to_owned())?;
    }
    rows.extend(unique.into_iter().map(|(key, (record, _))| (key, record)));
    finish_update(file_key, manifest, rows, encoded_bytes)
}

/// Admits a file manifest against its exact compact source row and resolves
/// every page before returning the closed complete-facts type.
///
/// `lookup` must read from the immutable facts relation root selected in the
/// same prepared workspace closure as the source row. Admission performs a
/// bounded validation pass; a paged result retains the lookup so projection
/// can visit pages again without hydrating the whole file or project.
///
/// # Errors
/// Returns an error for an identity mismatch, missing page, wrong owner,
/// unordered or inconsistent page tree, count mismatch, or content-key
/// mismatch.
pub fn admit_product_source_file_facts<Lookup>(
    file: ProductFileRef<'_>,
    manifest_key: [u8; 32],
    manifest: ProductSourceFileFactsManifest,
    mut lookup: Lookup,
) -> Result<ProductSourceFileFactsAdmission<Lookup>, String>
where
    Lookup: FnMut(&[u8; 32]) -> Result<Option<ProductSourceFileFactsRecord>, String>,
{
    let file_key = product_source_file_key(file.project, file.path);
    if manifest_key != file_key || !manifest.matches_file_identity(file) {
        return Err("source facts manifest does not bind the selected file row".to_owned());
    }
    match manifest.status {
        ProductSourceFileFactsStatus::InlineComplete(declarations) => {
            validate_declarations(file.path, &declarations)?;
            Ok(ProductSourceFileFactsAdmission::InlineComplete(
                ProductSourceFileFactsInline {
                    file_key,
                    declarations,
                },
            ))
        }
        ProductSourceFileFactsStatus::Paged {
            declaration_count,
            directories,
        } => {
            validate_paged_tree(
                &mut lookup,
                file_key,
                file.project,
                file.path,
                file.language,
                declaration_count,
                &directories,
            )?;
            Ok(ProductSourceFileFactsAdmission::PagedVerified(
                ProductSourceFileFactsPaged {
                    file_key,
                    project: file.project,
                    path: file.path.to_owned(),
                    language: file.language,
                    declaration_count,
                    directories,
                    lookup,
                },
            ))
        }
    }
}

/// Returns all canonical relation keys owned by one admitted file-facts
/// manifest. The manifest and every referenced directory/page are validated
/// against the exact compact source row before their keys can be removed or
/// replaced by a source transaction.
///
/// The returned vector is bounded by the single file's verified page tree;
/// it never walks or materializes other files in the project relation.
///
/// # Errors
/// Returns an error when the manifest is not bound to `file` or its page tree
/// is incomplete, unordered, misowned, or has a bad content key.
pub fn product_source_file_facts_row_keys<Lookup>(
    file: ProductFileRef<'_>,
    manifest_key: [u8; 32],
    manifest: ProductSourceFileFactsManifest,
    lookup: Lookup,
) -> Result<Vec<[u8; 32]>, String>
where
    Lookup: FnMut(&[u8; 32]) -> Result<Option<ProductSourceFileFactsRecord>, String>,
{
    let mut keys = vec![manifest_key];
    match admit_product_source_file_facts(file, manifest_key, manifest, lookup)? {
        ProductSourceFileFactsAdmission::Unavailable(_) => {
            return Err("unavailable source has no complete facts row keys".to_owned());
        }
        ProductSourceFileFactsAdmission::InlineComplete(_) => {}
        ProductSourceFileFactsAdmission::PagedVerified(mut paged) => {
            for directory_ref in paged.directories.iter() {
                let directory = lookup_directory(&mut paged.lookup, paged.file_key, directory_ref)?;
                keys.push(directory_ref.key);
                for page_ref in directory.pages.iter() {
                    let page = lookup_page(
                        &mut paged.lookup,
                        paged.file_key,
                        page_ref,
                        &directory_ref.key,
                    )?;
                    validate_page_identity(
                        &page,
                        paged.project,
                        &paged.path,
                        paged.language,
                        directory_ref
                            .base_line
                            .checked_add(page_ref.line_delta)
                            .ok_or_else(|| "facts page base line exceeds u32".to_owned())?,
                    )?;
                    keys.push(page_ref.key);
                }
            }
        }
    }
    Ok(keys)
}

impl Relation for ProductSourceFileFactsRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 6;
    const VERSION: u8 = 1;
    type Key = [u8; 32];
    type Value = ProductSourceFileFactsRecord;

    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        encode_record_infallible(value, output);
    }
}

impl CanonicalRelation for ProductSourceFileFactsRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes.try_into().map_err(|_| RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        decode_record(bytes).map_err(|()| RelationDecodeError::Malformed)
    }
}

/// Content key for one version-independent source declaration page.
#[must_use]
pub fn product_source_facts_page_key(
    file_key: [u8; 32],
    page: &ProductSourceFileFactsRecord,
) -> Result<[u8; 32], String> {
    if !matches!(page, ProductSourceFileFactsRecord::Page(_)) {
        return Err("facts page key input has the wrong record or owner".to_owned());
    }
    product_source_file_facts_record_key(file_key, page)
}

/// Returns the canonical relation key for one facts row owned by `file_key`.
/// Manifest keys are the source-file key; directory and declaration-page keys
/// are content hashes that include their exact file owner.
///
/// # Errors
/// Returns an error for a mismatched owner or a manifest stored under any key
/// other than its owning source-file key.
pub fn product_source_file_facts_record_key(
    file_key: [u8; 32],
    record: &ProductSourceFileFactsRecord,
) -> Result<[u8; 32], String> {
    if record.file_key() != Some(file_key) {
        return Err("facts record has the wrong file owner".to_owned());
    }
    match record {
        ProductSourceFileFactsRecord::Manifest(_) => Ok(file_key),
        ProductSourceFileFactsRecord::Directory(_) | ProductSourceFileFactsRecord::Page(_) => {
            record_content_key(record)
        }
    }
}

struct BuiltPage {
    key: [u8; 32],
    base_line: u32,
    first_boundary: [u8; 32],
    last_boundary: [u8; 32],
    record: ProductSourceFactsPage,
    encoded_bytes: usize,
}

fn finish_update(
    manifest_key: [u8; 32],
    manifest: ProductSourceFileFactsManifest,
    pages: Vec<([u8; 32], ProductSourceFileFactsRecord)>,
    encoded_bytes: usize,
) -> Result<ProductSourceFileFactsUpdate, String> {
    if encoded_bytes == 0 {
        return Err("source facts manifest exceeds canonical row capacity".to_owned());
    }
    Ok(ProductSourceFileFactsUpdate {
        manifest_key,
        manifest,
        pages: pages.into_boxed_slice(),
        encoded_bytes,
    })
}

fn build_declaration_pages(
    file_key: [u8; 32],
    project: [u8; 32],
    path: &str,
    language: SourceLanguage,
    declarations: &[SourceDeclaration],
    boundaries: &[[u8; 32]],
) -> Result<Vec<BuiltPage>, String> {
    let mut pages = Vec::new();
    let mut start = 0usize;
    while start < declarations.len() {
        let base_line = declarations[start].line();
        let mut end = start;
        let mut estimated = 128usize;
        while end < declarations.len() && end - start < MAX_DECLARATIONS_PER_PAGE {
            let cost = estimate_declaration_bytes(&declarations[end], path);
            if end > start
                && estimated.saturating_add(cost)
                    > ProductSourceFileFactsRecord::ROW_VALUE_CAPACITY
                        .saturating_sub(FACTS_PAGE_HEADROOM)
            {
                break;
            }
            if end == start
                && cost
                    > ProductSourceFileFactsRecord::ROW_VALUE_CAPACITY
                        .saturating_sub(FACTS_PAGE_HEADROOM)
            {
                return Err("one source declaration exceeds facts page capacity".to_owned());
            }
            estimated = estimated.saturating_add(cost);
            end += 1;
            let page_count = end - start;
            let is_content_cut =
                page_count >= MIN_CONTENT_CUT && boundaries[end - 1][0] & 0x1f == 0;
            if is_content_cut || page_count == MAX_DECLARATIONS_PER_PAGE {
                break;
            }
        }
        if end == start {
            return Err("source facts page planner made no progress".to_owned());
        }
        let mut record = ProductSourceFileFactsRecord::Page(build_page(
            file_key,
            project,
            path,
            language,
            base_line,
            &declarations[start..end],
            &boundaries[start..end],
        )?);
        let mut encoded = encode_record(&record);
        while encoded.len() > ProductSourceFileFactsRecord::ROW_VALUE_CAPACITY {
            if end <= start + 1 {
                return Err(
                    "one encoded source facts declaration page exceeds row capacity".to_owned(),
                );
            }
            end -= 1;
            record = ProductSourceFileFactsRecord::Page(build_page(
                file_key,
                project,
                path,
                language,
                base_line,
                &declarations[start..end],
                &boundaries[start..end],
            )?);
            encoded = encode_record(&record);
        }
        let key = content_key_bytes(&encoded);
        let ProductSourceFileFactsRecord::Page(page) = record else {
            return Err("facts page planner produced a non-page row".to_owned());
        };
        pages.push(BuiltPage {
            key,
            base_line,
            first_boundary: page.first_boundary,
            last_boundary: page.last_boundary,
            record: page,
            encoded_bytes: encoded.len(),
        });
        start = end;
    }
    Ok(pages)
}

fn build_page(
    file_key: [u8; 32],
    project: [u8; 32],
    path: &str,
    language: SourceLanguage,
    base_line: u32,
    declarations: &[SourceDeclaration],
    boundaries: &[[u8; 32]],
) -> Result<ProductSourceFactsPage, String> {
    if declarations.is_empty() || declarations.len() != boundaries.len() {
        return Err("source facts page has no declarations or mismatched boundaries".to_owned());
    }
    let mut relative = Vec::with_capacity(declarations.len());
    let mut container_line_deltas = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        let line = declaration
            .line()
            .checked_sub(base_line)
            .and_then(|line| line.checked_add(1))
            .ok_or_else(|| "source declaration line moved before its page base".to_owned())?;
        let (container, delta) = match declaration.container() {
            Container::Module => (Container::Module, None),
            Container::Attached { type_name } => (Container::attached(type_name), None),
            Container::Enclosing { name, line } => (
                Container::enclosing(name, NonZeroU32::MIN),
                Some(i64::from(line.get()) - i64::from(base_line)),
            ),
        };
        let location = SourceLocation::new(path, line)?;
        relative.push(
            SourceDeclaration::with_location(
                location,
                declaration.name(),
                declaration.kind(),
                declaration.signature(),
                declaration.documentation(),
            )
            .map_err(|error| format!("invalid source facts declaration: {error}"))?
            .with_source_excerpt(declaration.source_excerpt().clone())
            .with_container(container)
            .with_facts(declaration.facts().clone()),
        );
        container_line_deltas.push(delta);
    }
    Ok(ProductSourceFactsPage {
        file_key,
        project,
        path: path.to_owned(),
        language,
        first_boundary: boundaries[0],
        last_boundary: *boundaries
            .last()
            .ok_or_else(|| "source facts page has no last boundary".to_owned())?,
        declarations: Arc::from(relative.into_boxed_slice()),
        container_line_deltas: Arc::from(container_line_deltas.into_boxed_slice()),
    })
}

fn estimate_declaration_bytes(declaration: &SourceDeclaration, path: &str) -> usize {
    let excerpt = declaration.source_excerpt().text().map_or(0, str::len);
    let container = match declaration.container() {
        Container::Module => 1,
        Container::Attached { type_name }
        | Container::Enclosing {
            name: type_name, ..
        } => type_name.len(),
    };
    let text = declaration
        .name()
        .len()
        .saturating_add(path.len())
        .saturating_add(declaration.kind_name().len())
        .saturating_add(declaration.signature().len())
        .saturating_add(declaration.documentation().len())
        .saturating_add(excerpt)
        .saturating_add(container)
        .saturating_add(declaration.facts().text_bytes());
    // Every text field has a four-byte length and each typed fact/enum adds a
    // small fixed tag. The 64-byte allowance deliberately overestimates that
    // overhead before bounded page scratch is allocated.
    text.saturating_add(64)
}

fn validate_file_facts_input(path: &str, declarations: &[SourceDeclaration]) -> Result<(), String> {
    if path.is_empty() || path.len() > ProductSourceRecord::MAX_LABEL_BYTES {
        return Err("source facts path is empty or oversized".to_owned());
    }
    if declarations.len() > ProductSourceRecord::MAX_FILE_DECLARATIONS {
        return Err("source facts declaration count exceeds its limit".to_owned());
    }
    validate_declarations(path, declarations)
}

fn validate_declarations(path: &str, declarations: &[SourceDeclaration]) -> Result<(), String> {
    if declarations.len() > ProductSourceRecord::MAX_FILE_DECLARATIONS {
        return Err("source facts declaration count exceeds its limit".to_owned());
    }
    let mut prior_line = 0u32;
    for declaration in declarations {
        if declaration.location().path() != path || declaration.line() < prior_line {
            return Err("source facts declarations are not exact-path source order".to_owned());
        }
        if [
            declaration.name(),
            declaration.kind_name(),
            declaration.signature(),
            declaration.documentation(),
        ]
        .into_iter()
        .any(|text| text.len() > SourceDeclaration::MAX_TEXT_BYTES)
        {
            return Err("source facts declaration has oversized retained text".to_owned());
        }
        if declaration
            .source_excerpt()
            .text()
            .is_some_and(|text| text.is_empty() || text.len() > SourceExcerpt::MAX_BYTES)
        {
            return Err("source facts declaration has an invalid source excerpt".to_owned());
        }
        let container_name = match declaration.container() {
            Container::Module => None,
            Container::Enclosing { name, .. } | Container::Attached { type_name: name } => {
                Some(name.as_str())
            }
        };
        if container_name
            .is_some_and(|name| name.is_empty() || name.len() > SourceDeclaration::MAX_TEXT_BYTES)
        {
            return Err("source facts declaration has an invalid container name".to_owned());
        }
        if declaration
            .facts()
            .deprecation
            .present()
            .is_some_and(|notice| {
                [notice.since(), notice.note()]
                    .into_iter()
                    .flatten()
                    .any(|text| text.len() > MAX_FACT_TEXT_BYTES)
            })
        {
            return Err("source facts declaration has oversized fact text".to_owned());
        }
        prior_line = declaration.line();
        if let Container::Enclosing { line, .. } = declaration.container()
            && line.get() > declaration.line()
        {
            return Err("source declaration container starts after its declaration".to_owned());
        }
    }
    Ok(())
}

fn declaration_boundary_key(declaration: &SourceDeclaration) -> Result<[u8; 32], String> {
    let mut bytes = Vec::new();
    push_text(&mut bytes, declaration.name())?;
    bytes.push(declaration.kind().wire_tag());
    push_text(&mut bytes, declaration.signature())?;
    push_text(&mut bytes, declaration.documentation())?;
    encode_source_excerpt(declaration.source_excerpt(), &mut bytes)?;
    match declaration.container() {
        Container::Module => bytes.push(0),
        Container::Enclosing { name, .. } => {
            bytes.push(1);
            push_text(&mut bytes, name)?;
        }
        Container::Attached { type_name } => {
            bytes.push(2);
            push_text(&mut bytes, type_name)?;
        }
    }
    encode_facts_infallible(declaration.facts(), &mut bytes);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.product-source-facts.boundary.v1\0");
    hasher.update(&bytes);
    Ok(*hasher.finalize().as_bytes())
}

fn validate_paged_tree<Lookup>(
    lookup: &mut Lookup,
    file_key: [u8; 32],
    project: [u8; 32],
    path: &str,
    language: SourceLanguage,
    declaration_count: u32,
    directories: &[ProductSourceFactsDirectoryRef],
) -> Result<(), String>
where
    Lookup: FnMut(&[u8; 32]) -> Result<Option<ProductSourceFileFactsRecord>, String>,
{
    if declaration_count == 0
        || usize::try_from(declaration_count).unwrap_or(usize::MAX)
            > ProductSourceRecord::MAX_FILE_DECLARATIONS
        || directories.is_empty()
    {
        return Err("paged source facts manifest has an invalid full count".to_owned());
    }
    let mut total = 0usize;
    let mut previous_directory_line = None;
    let mut previous_declaration_line = None;
    for directory_ref in directories {
        if directory_ref.declaration_count == 0
            || directory_ref.page_count == 0
            || usize::from(directory_ref.page_count) > MAX_PAGES_PER_DIRECTORY
            || previous_directory_line.is_some_and(|line| line > directory_ref.base_line)
        {
            return Err("source facts directory references are unordered or empty".to_owned());
        }
        let directory = lookup_directory(lookup, file_key, directory_ref)?;
        if directory.pages.len() != usize::from(directory_ref.page_count) {
            return Err("source facts directory page count does not match its manifest".to_owned());
        }
        if directory
            .pages
            .first()
            .is_none_or(|page| page.first_boundary != directory_ref.first_boundary)
            || directory
                .pages
                .last()
                .is_none_or(|page| page.last_boundary != directory_ref.last_boundary)
        {
            return Err("source facts directory boundary does not match its manifest".to_owned());
        }
        let mut directory_count = 0usize;
        let mut prior_page_line = None;
        for page_ref in directory.pages.iter() {
            let page_base = directory_ref
                .base_line
                .checked_add(page_ref.line_delta)
                .ok_or_else(|| "facts page base line exceeds u32".to_owned())?;
            if prior_page_line.is_some_and(|line| line > page_base) {
                return Err("source facts page references are not source ordered".to_owned());
            }
            let page = lookup_page(lookup, file_key, page_ref, &directory_ref.key)?;
            validate_page_identity(&page, project, path, language, page_base)?;
            let page_count = page.declarations.len();
            if page_count != usize::from(page_ref.declaration_count) {
                return Err("source facts page count does not match its directory".to_owned());
            }
            if page.first_boundary != page_ref.first_boundary
                || page.last_boundary != page_ref.last_boundary
            {
                return Err("source facts page boundary does not match its directory".to_owned());
            }
            let first_boundary = declaration_boundary_key(
                page.declarations
                    .first()
                    .ok_or_else(|| "source facts page is empty".to_owned())?,
            )?;
            let last_boundary = declaration_boundary_key(
                page.declarations
                    .last()
                    .ok_or_else(|| "source facts page is empty".to_owned())?,
            )?;
            if first_boundary != page.first_boundary || last_boundary != page.last_boundary {
                return Err("source facts page boundary digest is invalid".to_owned());
            }
            let last_line = page_last_absolute_line(&page, page_base)?;
            if previous_declaration_line.is_some_and(|line| line > page_base) {
                return Err("source facts declarations move backwards across pages".to_owned());
            }
            previous_declaration_line = Some(last_line);
            prior_page_line = Some(page_base);
            directory_count = directory_count
                .checked_add(page_count)
                .ok_or_else(|| "source facts directory count overflowed".to_owned())?;
        }
        if directory_count != usize::try_from(directory_ref.declaration_count).unwrap_or(usize::MAX)
        {
            return Err("source facts directory declaration count is invalid".to_owned());
        }
        total = total
            .checked_add(directory_count)
            .ok_or_else(|| "source facts full declaration count overflowed".to_owned())?;
        previous_directory_line = Some(directory_ref.base_line);
    }
    if total != usize::try_from(declaration_count).unwrap_or(usize::MAX) {
        return Err("source facts manifest declaration count is invalid".to_owned());
    }
    Ok(())
}

fn lookup_directory<Lookup>(
    lookup: &mut Lookup,
    file_key: [u8; 32],
    reference: &ProductSourceFactsDirectoryRef,
) -> Result<ProductSourceFactsDirectory, String>
where
    Lookup: FnMut(&[u8; 32]) -> Result<Option<ProductSourceFileFactsRecord>, String>,
{
    let record = lookup(&reference.key)?
        .ok_or_else(|| "source facts directory is missing from the selected root".to_owned())?;
    let ProductSourceFileFactsRecord::Directory(directory) = record else {
        return Err("source facts directory key names another row kind".to_owned());
    };
    if directory.file_key != file_key
        || record_content_key(&ProductSourceFileFactsRecord::Directory(directory.clone()))?
            != reference.key
        || directory.pages.is_empty()
        || directory.pages.len() > MAX_PAGES_PER_DIRECTORY
    {
        return Err("source facts directory owner or content key is invalid".to_owned());
    }
    Ok(directory)
}

fn lookup_page<Lookup>(
    lookup: &mut Lookup,
    file_key: [u8; 32],
    reference: &ProductSourceFactsPageRef,
    directory_key: &[u8; 32],
) -> Result<ProductSourceFactsPage, String>
where
    Lookup: FnMut(&[u8; 32]) -> Result<Option<ProductSourceFileFactsRecord>, String>,
{
    let record = lookup(&reference.key)?.ok_or_else(|| {
        "source facts declaration page is missing from the selected root".to_owned()
    })?;
    let ProductSourceFileFactsRecord::Page(page) = record else {
        return Err("source facts page key names another row kind".to_owned());
    };
    if page.file_key != file_key
        || page.declarations.is_empty()
        || page.declarations.len() > MAX_DECLARATIONS_PER_PAGE
        || page.declarations.len() != page.container_line_deltas.len()
        || record_content_key(&ProductSourceFileFactsRecord::Page(page.clone()))? != reference.key
        || page.first_boundary != reference.first_boundary
        || page.last_boundary != reference.last_boundary
    {
        return Err(format!(
            "source facts page owner, order, or content key is invalid under directory {}",
            hex_key(directory_key)
        ));
    }
    Ok(page)
}

fn validate_page_identity(
    page: &ProductSourceFactsPage,
    project: [u8; 32],
    path: &str,
    language: SourceLanguage,
    base_line: u32,
) -> Result<(), String> {
    if page.project != project || page.path != path || page.language != language {
        return Err("source facts page does not match its file owner".to_owned());
    }
    let mut prior_line = 0u32;
    for (declaration, delta) in page
        .declarations
        .iter()
        .zip(page.container_line_deltas.iter())
    {
        if declaration.location().path() != path
            || declaration.line() < prior_line
            || declaration.line() == 0
        {
            return Err("source facts page declarations have invalid relative spans".to_owned());
        }
        prior_line = declaration.line();
        match (declaration.container(), delta) {
            (Container::Enclosing { line, .. }, Some(value)) if line.get() == 1 => {
                let absolute = i64::from(base_line)
                    .checked_add(*value)
                    .ok_or_else(|| "source facts container line overflowed".to_owned())?;
                let declaration_line = base_line
                    .checked_add(declaration.line().saturating_sub(1))
                    .ok_or_else(|| "source facts declaration line overflowed".to_owned())?;
                if absolute <= 0 || absolute > i64::from(declaration_line) {
                    return Err("source facts container line is outside its declaration".to_owned());
                }
            }
            (Container::Module | Container::Attached { .. }, None) => {}
            _ => return Err("source facts page container spans are malformed".to_owned()),
        }
    }
    if page
        .declarations
        .first()
        .is_none_or(|first| first.line() != 1)
    {
        return Err("source facts page does not start at its relative base".to_owned());
    }
    Ok(())
}

fn page_last_absolute_line(page: &ProductSourceFactsPage, base_line: u32) -> Result<u32, String> {
    let last = page
        .declarations
        .last()
        .ok_or_else(|| "source facts page is empty".to_owned())?;
    base_line
        .checked_add(last.line().saturating_sub(1))
        .ok_or_else(|| "source facts page span exceeds u32".to_owned())
}

fn record_content_key(record: &ProductSourceFileFactsRecord) -> Result<[u8; 32], String> {
    let bytes = encode_record(record);
    Ok(content_key_bytes(&bytes))
}

fn content_key_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.product-source-file-facts.row.v1\0");
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn encode_record(record: &ProductSourceFileFactsRecord) -> Vec<u8> {
    let mut output = Vec::new();
    encode_record_infallible(record, &mut output);
    output
}

fn encode_record_infallible(record: &ProductSourceFileFactsRecord, output: &mut Vec<u8>) {
    output.extend_from_slice(MAGIC);
    match record {
        ProductSourceFileFactsRecord::Manifest(manifest) => {
            output.push(MANIFEST_TAG);
            encode_manifest_payload(manifest, output);
        }
        ProductSourceFileFactsRecord::Directory(directory) => {
            output.push(DIRECTORY_TAG);
            output.extend_from_slice(&directory.file_key);
            push_count_infallible(output, directory.pages.len());
            for page in directory.pages.iter() {
                encode_page_ref(page, output);
            }
        }
        ProductSourceFileFactsRecord::Page(page) => {
            output.push(DECLARATION_PAGE_TAG);
            output.extend_from_slice(&page.file_key);
            output.extend_from_slice(&page.project);
            push_text_infallible(output, &page.path);
            output.push(page.language.wire_tag());
            output.extend_from_slice(&page.first_boundary);
            output.extend_from_slice(&page.last_boundary);
            push_count_infallible(output, page.declarations.len());
            for (declaration, container_delta) in page
                .declarations
                .iter()
                .zip(page.container_line_deltas.iter())
            {
                encode_declaration_infallible(declaration, output);
                match container_delta {
                    Some(delta) => {
                        output.push(1);
                        output.extend_from_slice(&delta.to_be_bytes());
                    }
                    None => output.push(0),
                }
            }
        }
    }
}

fn encode_manifest(manifest: &ProductSourceFileFactsManifest) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(MAGIC);
    output.push(MANIFEST_TAG);
    encode_manifest_payload(manifest, &mut output);
    output
}

fn encode_manifest_payload(manifest: &ProductSourceFileFactsManifest, output: &mut Vec<u8>) {
    output.extend_from_slice(&manifest.project);
    push_text_infallible(output, &manifest.path);
    output.extend_from_slice(&manifest.content_version);
    output.extend_from_slice(&manifest.analysis_version);
    output.extend_from_slice(manifest.source_identity.as_ref());
    match &manifest.status {
        ProductSourceFileFactsStatus::InlineComplete(declarations) => {
            output.push(0);
            push_count_infallible(output, declarations.len());
            for declaration in declarations.iter() {
                encode_declaration_infallible(declaration, output);
            }
        }
        ProductSourceFileFactsStatus::Paged {
            declaration_count,
            directories,
        } => {
            output.push(1);
            output.extend_from_slice(&declaration_count.to_be_bytes());
            push_count_infallible(output, directories.len());
            for directory in directories.iter() {
                encode_directory_ref(directory, output);
            }
        }
    }
}

fn encode_directory_ref(reference: &ProductSourceFactsDirectoryRef, output: &mut Vec<u8>) {
    output.extend_from_slice(&reference.key);
    output.extend_from_slice(&reference.base_line.to_be_bytes());
    output.extend_from_slice(&reference.declaration_count.to_be_bytes());
    output.extend_from_slice(&reference.page_count.to_be_bytes());
    output.extend_from_slice(&reference.first_boundary);
    output.extend_from_slice(&reference.last_boundary);
}

fn encode_page_ref(reference: &ProductSourceFactsPageRef, output: &mut Vec<u8>) {
    output.extend_from_slice(&reference.key);
    output.extend_from_slice(&reference.line_delta.to_be_bytes());
    output.extend_from_slice(&reference.declaration_count.to_be_bytes());
    output.extend_from_slice(&reference.first_boundary);
    output.extend_from_slice(&reference.last_boundary);
}

fn encode_declaration_infallible(declaration: &SourceDeclaration, output: &mut Vec<u8>) {
    output.extend_from_slice(&declaration.line().to_be_bytes());
    push_text_infallible(output, declaration.location().path());
    push_text_infallible(output, declaration.name());
    output.push(declaration.kind().wire_tag());
    push_text_infallible(output, declaration.signature());
    push_text_infallible(output, declaration.documentation());
    encode_source_excerpt_infallible(declaration.source_excerpt(), output);
    encode_container_infallible(declaration.container(), output);
    encode_facts_infallible(declaration.facts(), output);
}

fn encode_source_excerpt(excerpt: &SourceExcerpt, output: &mut Vec<u8>) -> Result<(), String> {
    encode_source_excerpt_infallible(excerpt, output);
    Ok(())
}

fn encode_source_excerpt_infallible(excerpt: &SourceExcerpt, output: &mut Vec<u8>) {
    match excerpt {
        SourceExcerpt::NotCaptured => output.push(0),
        SourceExcerpt::Captured { text, extent } => {
            output.push(match extent {
                SourceExcerptExtent::Complete => 1,
                SourceExcerptExtent::Truncated => 2,
            });
            push_text_infallible(output, text);
        }
        SourceExcerpt::NotHydrated => output.push(3),
        SourceExcerpt::Unconfigured => output.push(4),
    }
}

fn encode_container_infallible(container: &Container, output: &mut Vec<u8>) {
    match container {
        Container::Module => output.push(0),
        Container::Enclosing { name, line } => {
            output.push(1);
            push_text_infallible(output, name);
            output.extend_from_slice(&line.get().to_be_bytes());
        }
        Container::Attached { type_name } => {
            output.push(2);
            push_text_infallible(output, type_name);
        }
    }
}

fn encode_facts_infallible(facts: &DeclarationFacts, output: &mut Vec<u8>) {
    match &facts.deprecation {
        Fact::Unobserved => output.push(0),
        Fact::Absent => output.push(1),
        Fact::Present(value) => {
            output.push(2);
            push_optional_text(output, value.since());
            push_optional_text(output, value.note());
        }
    }
    match &facts.obligation {
        Fact::Unobserved => output.push(0),
        Fact::Absent => output.push(1),
        Fact::Present(value) => {
            output.push(2);
            output.push(value.wire_tag());
        }
    }
}

fn decode_record(bytes: &[u8]) -> Result<ProductSourceFileFactsRecord, ()> {
    let mut reader = FactsReader::new(bytes);
    if reader.take(4)? != MAGIC {
        return Err(());
    }
    let record = match reader.byte()? {
        MANIFEST_TAG => {
            let project = reader.array()?;
            let path = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let content_version = reader.array()?;
            let analysis_version = reader.array()?;
            let source_identity =
                ContentId::<SourceFactDomain>::try_from(reader.array()?).map_err(|_| ())?;
            let status = match reader.byte()? {
                0 => {
                    let count =
                        reader.count_items(ProductSourceRecord::MAX_FILE_DECLARATIONS, 25)?;
                    let mut declarations = Vec::with_capacity(count);
                    for _ in 0..count {
                        declarations.push(decode_declaration(&mut reader)?);
                    }
                    ProductSourceFileFactsStatus::InlineComplete(Arc::from(declarations))
                }
                1 => {
                    let declaration_count = reader.u32()?;
                    let count =
                        reader.count_items(ProductSourceRecord::MAX_FILE_DECLARATIONS, 106)?;
                    let mut directories = Vec::with_capacity(count);
                    for _ in 0..count {
                        directories.push(decode_directory_ref(&mut reader)?);
                    }
                    ProductSourceFileFactsStatus::Paged {
                        declaration_count,
                        directories: Arc::from(directories),
                    }
                }
                _ => return Err(()),
            };
            if path.is_empty() {
                return Err(());
            }
            ProductSourceFileFactsRecord::Manifest(ProductSourceFileFactsManifest {
                project,
                path,
                content_version,
                analysis_version,
                source_identity,
                status,
            })
        }
        DIRECTORY_TAG => {
            let file_key = reader.array()?;
            let count = reader.count_items(MAX_PAGES_PER_DIRECTORY, 102)?;
            let mut pages = Vec::with_capacity(count);
            for _ in 0..count {
                pages.push(decode_page_ref(&mut reader)?);
            }
            ProductSourceFileFactsRecord::Directory(ProductSourceFactsDirectory {
                file_key,
                pages: Arc::from(pages),
            })
        }
        DECLARATION_PAGE_TAG => {
            let file_key = reader.array()?;
            let project = reader.array()?;
            let path = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let language = SourceLanguage::from_wire_tag(reader.byte()?).ok_or(())?;
            let first_boundary = reader.array()?;
            let last_boundary = reader.array()?;
            let count = reader.count_items(MAX_DECLARATIONS_PER_PAGE, 26)?;
            let mut declarations = Vec::with_capacity(count);
            let mut container_line_deltas = Vec::with_capacity(count);
            for _ in 0..count {
                declarations.push(decode_declaration(&mut reader)?);
                container_line_deltas.push(match reader.byte()? {
                    0 => None,
                    1 => Some(reader.i64()?),
                    _ => return Err(()),
                });
            }
            ProductSourceFileFactsRecord::Page(ProductSourceFactsPage {
                file_key,
                project,
                path,
                language,
                first_boundary,
                last_boundary,
                declarations: Arc::from(declarations),
                container_line_deltas: Arc::from(container_line_deltas),
            })
        }
        _ => return Err(()),
    };
    reader.finish()?;
    Ok(record)
}

fn decode_directory_ref(
    reader: &mut FactsReader<'_>,
) -> Result<ProductSourceFactsDirectoryRef, ()> {
    Ok(ProductSourceFactsDirectoryRef {
        key: reader.array()?,
        base_line: reader.u32()?,
        declaration_count: reader.u32()?,
        page_count: reader.u16()?,
        first_boundary: reader.array()?,
        last_boundary: reader.array()?,
    })
}

fn decode_page_ref(reader: &mut FactsReader<'_>) -> Result<ProductSourceFactsPageRef, ()> {
    Ok(ProductSourceFactsPageRef {
        key: reader.array()?,
        line_delta: reader.u32()?,
        declaration_count: reader.u16()?,
        first_boundary: reader.array()?,
        last_boundary: reader.array()?,
    })
}

fn decode_declaration(reader: &mut FactsReader<'_>) -> Result<SourceDeclaration, ()> {
    let line = NonZeroU32::new(reader.u32()?).ok_or(())?;
    let path = reader.text(SourceLocation::MAX_PATH_BYTES)?;
    let name = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
    let kind = DeclarationKind::from_wire_tag(reader.byte()?).ok_or(())?;
    let signature = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
    let documentation = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
    let excerpt_tag = reader.byte()?;
    let excerpt = match excerpt_tag {
        0 => SourceExcerpt::NotCaptured,
        1 | 2 => {
            let text = reader.text(SourceExcerpt::MAX_BYTES)?;
            SourceExcerpt::captured(
                &text,
                if excerpt_tag == 1 {
                    SourceExcerptExtent::Complete
                } else {
                    SourceExcerptExtent::Truncated
                },
            )
            .map_err(|_| ())?
        }
        3 => SourceExcerpt::NotHydrated,
        4 => SourceExcerpt::Unconfigured,
        _ => return Err(()),
    };
    let container = match reader.byte()? {
        0 => Container::Module,
        1 => {
            let name = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
            let line = NonZeroU32::new(reader.u32()?).ok_or(())?;
            Container::enclosing(&name, line)
        }
        2 => Container::attached(&reader.text(SourceDeclaration::MAX_TEXT_BYTES)?),
        _ => return Err(()),
    };
    let facts = decode_facts(reader)?;
    Ok(SourceDeclaration::with_location(
        SourceLocation::new(path, line.get()).map_err(|_| ())?,
        name,
        kind,
        signature,
        documentation,
    )
    .map_err(|_| ())?
    .with_source_excerpt(excerpt)
    .with_container(container)
    .with_facts(facts))
}

fn decode_facts(reader: &mut FactsReader<'_>) -> Result<DeclarationFacts, ()> {
    let deprecation = match reader.byte()? {
        0 => Fact::Unobserved,
        1 => Fact::Absent,
        2 => {
            let since = reader.optional_text(MAX_FACT_TEXT_BYTES)?;
            let note = reader.optional_text(MAX_FACT_TEXT_BYTES)?;
            Fact::Present(Deprecation::admit(since, note).map_err(|_| ())?)
        }
        _ => return Err(()),
    };
    let obligation = match reader.byte()? {
        0 => Fact::Unobserved,
        1 => Fact::Absent,
        2 => Fact::Present(Obligation::from_wire_tag(reader.byte()?).map_err(|_| ())?),
        _ => return Err(()),
    };
    Ok(DeclarationFacts {
        deprecation,
        obligation,
    })
}

fn push_text(output: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let length =
        u32::try_from(value.len()).map_err(|_| "source facts text exceeds u32".to_owned())?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn push_text_infallible(output: &mut Vec<u8>, value: &str) {
    let length = u32::try_from(value.len()).unwrap_or(u32::MAX);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
}

fn push_count_infallible(output: &mut Vec<u8>, count: usize) {
    output.extend_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_be_bytes());
}

fn push_optional_text(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            push_text_infallible(output, value);
        }
        None => output.push(0),
    }
}

fn hex_key(key: &[u8; 32]) -> String {
    key.iter().map(|byte| format!("{byte:02x}")).collect()
}

struct FactsReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> FactsReader<'a> {
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

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ()> {
        self.take(N)?.try_into().map_err(|_| ())
    }

    fn u16(&mut self) -> Result<u16, ()> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, ()> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, ()> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    fn count(&mut self, maximum: usize) -> Result<usize, ()> {
        let count = usize::try_from(self.u32()?).map_err(|_| ())?;
        (count <= maximum).then_some(count).ok_or(())
    }

    /// Reads a bounded vector length only when the remaining canonical row
    /// could physically encode every minimum-sized item. This prevents a tiny
    /// malformed row from asking `Vec::with_capacity` for the full admission
    /// ceiling before the first item is decoded.
    fn count_items(&mut self, maximum: usize, minimum_item_bytes: usize) -> Result<usize, ()> {
        let count = self.count(maximum)?;
        if minimum_item_bytes == 0
            || count > self.bytes.len().saturating_sub(self.at) / minimum_item_bytes
        {
            return Err(());
        }
        Ok(count)
    }

    fn text(&mut self, maximum: usize) -> Result<String, ()> {
        let length = self.count(maximum)?;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| ())
    }

    fn optional_text(&mut self, maximum: usize) -> Result<Option<String>, ()> {
        match self.byte()? {
            0 => Ok(None),
            1 => self.text(maximum).map(Some),
            _ => Err(()),
        }
    }

    fn finish(&self) -> Result<(), ()> {
        (self.at == self.bytes.len()).then_some(()).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    #[test]
    fn malformed_tiny_count_is_rejected_before_vector_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(MANIFEST_TAG);
        bytes.extend_from_slice(&[1; 32]);
        push_text_infallible(&mut bytes, "x");
        bytes.extend_from_slice(&[2; 32]);
        bytes.extend_from_slice(&[3; 32]);
        bytes.extend_from_slice(
            ContentId::<SourceFactDomain>::from_canonical_bytes(b"source").as_ref(),
        );
        bytes.push(0);
        bytes.extend_from_slice(
            &u32::try_from(ProductSourceRecord::MAX_FILE_DECLARATIONS)
                .expect("bounded count")
                .to_be_bytes(),
        );
        assert!(ProductSourceFileFactsRelation::decode_value(&bytes).is_err());

        let mut reader = FactsReader::new(&[0, 0, 0, 1]);
        assert!(reader.count_items(16_384, 25).is_err());
    }

    fn declaration(path: &str, name: &str, line: u32) -> SourceDeclaration {
        declaration_with_container(path, name, line, 1)
    }

    fn declaration_with_container(
        path: &str,
        name: &str,
        line: u32,
        container_line: u32,
    ) -> SourceDeclaration {
        SourceDeclaration::at_path(
            path,
            name,
            DeclarationKind::Function,
            line,
            format!("fn {name}()"),
            format!("docs for {name}"),
        )
        .expect("declaration")
        .with_container(Container::enclosing(
            "Collection",
            NonZeroU32::new(container_line).expect("non-zero line"),
        ))
        .with_facts(DeclarationFacts {
            deprecation: Fact::Present(Deprecation::new(Some("2.0"), Some("use next"))),
            obligation: Fact::Present(Obligation::Required),
        })
    }

    fn source_row(
        project: [u8; 32],
        path: &str,
        content_version: [u8; 32],
        analysis_version: [u8; 32],
        source_identity: ContentId<SourceFactDomain>,
        declarations: Vec<SourceDeclaration>,
    ) -> ProductSourceRecord {
        ProductSourceRecord::file_within_row_capacity(
            project,
            path,
            SourceLanguage::Rust,
            content_version,
            analysis_version,
            declarations,
        )
        .expect("compact source row")
        .with_source_identity(source_identity)
        .expect("source identity")
    }

    #[test]
    fn complete_manifest_is_bound_to_exact_source_analysis() {
        let project = [7; 32];
        let path = "src/lib.rs";
        let declarations = vec![
            declaration(path, "first", 2),
            declaration(path, "second", 9),
        ];
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(b"source v1");
        let update = build_product_source_file_facts(
            project,
            path,
            SourceLanguage::Rust,
            [3; 32],
            [4; 32],
            identity,
            &declarations,
        )
        .expect("facts update");
        let source = source_row(
            project,
            path,
            [3; 32],
            [4; 32],
            identity,
            declarations.clone(),
        );
        let file = source.file_fields().expect("file fields");
        let ProductSourceFileFactsRecord::Manifest(manifest) =
            ProductSourceFileFactsRelation::decode_value(&encode_record(
                &ProductSourceFileFactsRecord::Manifest(update.manifest.clone()),
            ))
            .expect("canonical manifest")
        else {
            panic!("manifest record")
        };
        let admission =
            admit_product_source_file_facts(file, update.manifest_key(), manifest.clone(), |_| {
                Ok(None)
            })
            .expect("admit complete facts");
        match admission {
            ProductSourceFileFactsAdmission::InlineComplete(admitted) => {
                assert_eq!(admitted.declarations(), declarations);
            }
            ProductSourceFileFactsAdmission::Unavailable(_) => {
                panic!("complete fixture was unavailable")
            }
            ProductSourceFileFactsAdmission::PagedVerified(_) => panic!("small set is inline"),
        }
        let wrong_file = source_row(project, path, [9; 32], [4; 32], identity, declarations);
        let error = match admit_product_source_file_facts(
            wrong_file.file_fields().expect("file fields"),
            update.manifest_key(),
            manifest,
            |_| Ok(None),
        ) {
            Err(error) => error,
            Ok(_) => panic!("stale source manifest must be refused"),
        };
        assert!(error.contains("does not bind"));
    }

    #[test]
    fn paged_facts_reuse_source_producer_pages_after_a_prefix_insertion() {
        let project = [5; 32];
        let frontend = backend_frontend_typescript::syntax_frontend().expect("TypeScript frontend");
        let source = tsx_catalog_source((0..900), false);
        let original_analysis = frontend
            .analyze(Path::new("src/Panels.tsx"), source.as_bytes())
            .expect("realistic TSX source analysis");
        let original = original_analysis.declarations().to_vec();
        assert!(original.iter().any(|declaration| {
            declaration.kind() == DeclarationKind::Function
                && matches!(declaration.source_excerpt(), SourceExcerpt::Captured { .. })
                && !declaration.documentation().is_empty()
        }));
        let first = build_product_source_file_facts(
            project,
            "src/Panels.tsx",
            SourceLanguage::TypeScript,
            [1; 32],
            [2; 32],
            ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes()),
            &original,
        )
        .expect("initial facts");
        let prefixed_source = tsx_catalog_source((0..900), true);
        let prefixed_analysis = frontend
            .analyze(Path::new("src/Panels.tsx"), prefixed_source.as_bytes())
            .expect("prefixed realistic TSX source analysis");
        let changed = prefixed_analysis.declarations().to_vec();
        let second = build_product_source_file_facts(
            project,
            "src/Panels.tsx",
            SourceLanguage::TypeScript,
            [8; 32],
            [2; 32],
            ContentId::<SourceFactDomain>::from_canonical_bytes(prefixed_source.as_bytes()),
            &changed,
        )
        .expect("updated facts");
        assert_eq!(
            first.manifest().source_identity(),
            ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes())
        );
        assert_eq!(
            second.manifest().source_identity(),
            ContentId::<SourceFactDomain>::from_canonical_bytes(prefixed_source.as_bytes())
        );
        assert_ne!(
            first.manifest().source_identity(),
            second.manifest().source_identity()
        );
        let old_by_name = original
            .iter()
            .map(|declaration| (declaration.name(), declaration))
            .collect::<BTreeMap<_, _>>();
        for declaration in &changed {
            let Some(old) = old_by_name.get(declaration.name()) else {
                continue;
            };
            if declaration.kind() == DeclarationKind::Module {
                assert_eq!(
                    declaration.line(),
                    old.line(),
                    "module anchor stays at line one"
                );
                continue;
            }
            assert_eq!(
                declaration.line(),
                old.line() + 2,
                "only absolute lines shift"
            );
            assert_eq!(declaration.signature(), old.signature());
            assert_eq!(declaration.documentation(), old.documentation());
            assert_eq!(declaration.source_excerpt(), old.source_excerpt());
            assert_eq!(declaration.facts(), old.facts());
            assert_eq!(declaration.container(), old.container());
        }
        let old_keys = first
            .pages()
            .iter()
            .filter_map(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_)).then_some(*key)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let prefixed_keys = second
            .pages()
            .iter()
            .filter_map(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_)).then_some(*key)
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            old_keys, prefixed_keys,
            "line-only prefix must preserve every page key"
        );
        assert_eq!(second.declaration_count(), original.len());
        let compact = source_row(
            project,
            "src/Panels.tsx",
            [8; 32],
            [2; 32],
            ContentId::<SourceFactDomain>::from_canonical_bytes(prefixed_source.as_bytes()),
            changed.clone(),
        );
        let stored_rows = second.pages().iter().cloned().collect::<BTreeMap<_, _>>();
        let mut admitted = match admit_product_source_file_facts(
            compact.file_fields().expect("compact source row"),
            second.manifest_key(),
            second.manifest().clone(),
            move |key| Ok(stored_rows.get(key).cloned()),
        )
        .expect("admit actual paged TSX facts")
        {
            ProductSourceFileFactsAdmission::PagedVerified(paged) => paged,
            ProductSourceFileFactsAdmission::Unavailable(_) => {
                panic!("complete fixture was unavailable")
            }
            ProductSourceFileFactsAdmission::InlineComplete(_) => {
                panic!("900 declaration TSX inventory must use bounded pages")
            }
        };
        assert_eq!(admitted.declaration_count() as usize, changed.len());
        let mut visited = 0usize;
        let mut last_line = 0;
        admitted
            .visit_pages(|page| {
                for (index, declaration) in page.declarations.iter().enumerate() {
                    let line = page
                        .declaration_line(index)
                        .ok_or_else(|| "admitted TSX declaration line".to_owned())?;
                    if line < last_line {
                        return Err("admitted TSX declaration order".to_owned());
                    }
                    last_line = line;
                    if declaration.documentation().is_empty()
                        || declaration.source_excerpt().text().is_none()
                    {
                        return Err("admitted TSX declaration lost captured facts".to_owned());
                    }
                    visited += 1;
                }
                Ok(())
            })
            .expect("visit every verified TSX page");
        assert_eq!(visited, changed.len());
        let prefix_page_bytes = first
            .pages()
            .iter()
            .filter(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_))
                    && prefixed_keys.contains(key)
            })
            .map(|(_, record)| encode_record(record).len())
            .sum::<usize>();

        let comment_source = format!("// A harmless inventory note.\n\n{source}");
        let comment_analysis = frontend
            .analyze(Path::new("src/Panels.tsx"), comment_source.as_bytes())
            .expect("comment-prefixed realistic TSX source analysis");
        let comment_update = build_product_source_file_facts(
            project,
            "src/Panels.tsx",
            SourceLanguage::TypeScript,
            [0x18; 32],
            [2; 32],
            ContentId::<SourceFactDomain>::from_canonical_bytes(comment_source.as_bytes()),
            comment_analysis.declarations(),
        )
        .expect("comment-prefixed facts");
        let comment_keys = comment_update
            .pages()
            .iter()
            .filter_map(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_)).then_some(*key)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let original_first_page = first.pages().iter().find_map(|(key, record)| {
            let ProductSourceFileFactsRecord::Page(page) = record else {
                return None;
            };
            page.declarations
                .iter()
                .any(|declaration| declaration.name() == "Panel_0000")
                .then_some(*key)
        });
        let comment_first_page = comment_update.pages().iter().find_map(|(key, record)| {
            let ProductSourceFileFactsRecord::Page(page) = record else {
                return None;
            };
            page.declarations
                .iter()
                .any(|declaration| declaration.name() == "Panel_0000")
                .then_some(*key)
        });
        let original_first_page = original_first_page.expect("first declaration page");
        let comment_first_page = comment_first_page.expect("comment-prefixed first page");
        assert_ne!(original_first_page, comment_first_page);
        let rewritten_old = old_keys
            .difference(&comment_keys)
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let original_keys = first
            .pages()
            .iter()
            .filter_map(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_)).then_some(*key)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let rewritten_new = comment_keys
            .difference(&original_keys)
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(rewritten_old, [original_first_page].into());
        assert_eq!(rewritten_new, [comment_first_page].into());
        let original_page_bytes = first
            .pages()
            .iter()
            .filter(|(_, record)| matches!(record, ProductSourceFileFactsRecord::Page(_)))
            .map(|(_, record)| encode_record(record).len())
            .sum::<usize>();
        let comment_reused_bytes = first
            .pages()
            .iter()
            .filter(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_))
                    && comment_keys.contains(key)
            })
            .map(|(_, record)| encode_record(record).len())
            .sum::<usize>();
        eprintln!(
            "TSX comment prefix: rewrote 1 first declaration page; reused {}/{} prior page bytes",
            comment_reused_bytes, original_page_bytes
        );

        // Add a real source declaration in the middle, then exercise the same
        // frontend and page producer. Content-defined boundaries must
        // resynchronize shortly after the insertion instead of rewriting the
        // entire suffix.
        let mut inserted_source = prefixed_source.clone();
        let insert_at = inserted_source
            .find("/** Catalog panel 450;")
            .expect("middle declaration anchor");
        inserted_source.insert_str(
            insert_at,
            "/** Inserted panel. */\nexport function Panel_0449_inserted() { return <aside>Inserted</aside>; }\n",
        );
        let inserted_analysis = frontend
            .analyze(Path::new("src/Panels.tsx"), inserted_source.as_bytes())
            .expect("TSX declaration insertion analysis");
        let inserted = build_product_source_file_facts(
            project,
            "src/Panels.tsx",
            SourceLanguage::TypeScript,
            [9; 32],
            [2; 32],
            ContentId::<SourceFactDomain>::from_canonical_bytes(inserted_source.as_bytes()),
            inserted_analysis.declarations(),
        )
        .expect("inserted declaration facts");
        let inserted_keys = inserted
            .pages()
            .iter()
            .filter_map(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_)).then_some(*key)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let reused_keys = old_keys.intersection(&inserted_keys).count();
        let old_page_bytes = first
            .pages()
            .iter()
            .filter(|(_, record)| matches!(record, ProductSourceFileFactsRecord::Page(_)))
            .map(|(_, record)| encode_record(record).len())
            .sum::<usize>();
        let reused_bytes = first
            .pages()
            .iter()
            .filter(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_))
                    && inserted_keys.contains(key)
            })
            .map(|(_, record)| encode_record(record).len())
            .sum::<usize>();
        let rewritten_bytes = first
            .pages()
            .iter()
            .filter(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_))
                    && !inserted_keys.contains(key)
            })
            .chain(inserted.pages().iter().filter(|(key, record)| {
                matches!(record, ProductSourceFileFactsRecord::Page(_)) && !old_keys.contains(key)
            }))
            .map(|(_, record)| encode_record(record).len())
            .sum::<usize>();
        let old_rewritten_pages = old_keys.difference(&inserted_keys).count();
        let new_rewritten_pages = inserted_keys.difference(&old_keys).count();
        assert!(reused_keys > 0, "declaration insertion should reuse pages");
        assert!(
            reused_bytes * 2 >= old_page_bytes,
            "at least half of prior fact-page bytes should be reused"
        );
        assert!(
            old_rewritten_pages <= 4 && new_rewritten_pages <= 4,
            "one inserted declaration should rewrite only the affected content-cut neighborhood"
        );
        assert!(rewritten_bytes > 0);
        eprintln!(
            "TSX prefix edit: all {} pages / {prefix_page_bytes} bytes reused; one declaration insertion reused {reused_keys} pages / {reused_bytes} of {old_page_bytes} prior bytes and rewrote {rewritten_bytes} bytes",
            old_keys.len()
        );
    }

    fn tsx_catalog_source(indices: impl IntoIterator<Item = usize>, prefix: bool) -> String {
        let mut source = String::new();
        if prefix {
            source.push_str("\n\n");
        }
        for index in indices {
            source.push_str(&format!(
                "/** Catalog panel {index}; @deprecated use the next reviewed panel. */\n\
                 export function Panel_{index:04}({{ title, children }}: {{ title: string; children?: React.ReactNode }}) {{\n\
                   return <article data-panel=\"{index}\" aria-label={{title}}>{{children}}<h2>{{title}}</h2><button type=\"button\" onClick={{() => window.dispatchEvent(new CustomEvent(\"nudox:open\", {{ detail: title }}))}}>Open</button></article>;\n\
                 }}\n"
            ));
        }
        source
    }

    #[test]
    fn largest_valid_declaration_fields_still_form_a_complete_canonical_page() {
        let project = [0x17; 32];
        let path = "src/maximal.ts";
        let text = "x".repeat(SourceDeclaration::MAX_TEXT_BYTES);
        let excerpt = "e".repeat(SourceExcerpt::MAX_BYTES);
        let fact_text = "f".repeat(MAX_FACT_TEXT_BYTES);
        let deprecation = Deprecation::admit(Some(fact_text.clone()), Some(fact_text))
            .expect("maximal bounded fact text");
        let declarations = (1..=4)
            .map(|line| {
                SourceDeclaration::at_path(
                    path,
                    text.clone(),
                    DeclarationKind::Function,
                    line,
                    text.clone(),
                    text.clone(),
                )
                .expect("bounded declaration")
                .with_source_excerpt(
                    SourceExcerpt::captured(&excerpt, SourceExcerptExtent::Complete)
                        .expect("maximal bounded excerpt"),
                )
                .with_container(Container::attached(&text))
                .with_facts(DeclarationFacts {
                    deprecation: Fact::Present(deprecation.clone()),
                    obligation: Fact::Present(Obligation::Required),
                })
            })
            .collect::<Vec<_>>();
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(b"maximal source");
        let update = build_product_source_file_facts(
            project,
            path,
            SourceLanguage::TypeScript,
            [0x21; 32],
            [0x22; 32],
            identity,
            &declarations,
        )
        .expect("valid maximum-sized declaration facts");
        assert!(update.pages().iter().all(|(_, row)| {
            encode_record(row).len() <= ProductSourceFileFactsRecord::ROW_VALUE_CAPACITY
        }));
        let compact = source_row(
            project,
            path,
            [0x21; 32],
            [0x22; 32],
            identity,
            declarations.clone(),
        );
        let stored_rows = update.pages().iter().cloned().collect::<BTreeMap<_, _>>();
        let mut admitted = match admit_product_source_file_facts(
            compact
                .file_fields()
                .expect("compact maximum-sized source row"),
            update.manifest_key(),
            update.manifest().clone(),
            move |key| Ok(stored_rows.get(key).cloned()),
        )
        .expect("admit maximum-sized declaration pages")
        {
            ProductSourceFileFactsAdmission::PagedVerified(paged) => paged,
            ProductSourceFileFactsAdmission::Unavailable(_) => {
                panic!("complete fixture was unavailable")
            }
            ProductSourceFileFactsAdmission::InlineComplete(_) => {
                panic!("maximum-sized facts must use pages")
            }
        };
        let mut visited = Vec::new();
        admitted
            .visit_pages(|page| {
                for (index, declaration) in page.declarations.iter().enumerate() {
                    visited.push((
                        declaration.name().to_owned(),
                        page.declaration_line(index)
                            .ok_or_else(|| "maximal declaration line".to_owned())?,
                        declaration.signature().len(),
                        declaration.documentation().len(),
                        declaration.source_excerpt().text().map(str::len),
                    ));
                }
                Ok(())
            })
            .expect("visit maximum-sized pages");
        assert_eq!(visited.len(), declarations.len());
        assert!(visited.iter().all(|(name, _, signature, docs, excerpt)| {
            name.len() == SourceDeclaration::MAX_TEXT_BYTES
                && *signature == SourceDeclaration::MAX_TEXT_BYTES
                && *docs == SourceDeclaration::MAX_TEXT_BYTES
                && *excerpt == Some(SourceExcerpt::MAX_BYTES)
        }));
    }

    #[test]
    fn page_owner_count_and_content_key_are_checked_before_projection() {
        let project = [6; 32];
        let path = "src/lib.rs";
        let declarations = (0..1_800)
            .map(|index| declaration(path, &format!("entry_{index:04}"), index * 2 + 1))
            .collect::<Vec<_>>();
        let update = build_product_source_file_facts(
            project,
            path,
            SourceLanguage::Rust,
            [2; 32],
            [3; 32],
            ContentId::<SourceFactDomain>::from_canonical_bytes(b"source"),
            &declarations,
        )
        .expect("paged facts");
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(b"source");
        let source = source_row(project, path, [2; 32], [3; 32], identity, declarations);
        let mut rows = update.pages().iter().cloned().collect::<BTreeMap<_, _>>();
        let stale = source.file_fields().expect("file fields");
        let mut corrupt_key = None;
        for (key, record) in &mut rows {
            if let ProductSourceFileFactsRecord::Page(page) = record {
                page.file_key = [0xee; 32];
                corrupt_key = Some(*key);
                break;
            }
        }
        let result = admit_product_source_file_facts(
            stale,
            update.manifest_key(),
            update.manifest().clone(),
            move |key| Ok(rows.get(key).cloned()),
        );
        assert!(result.is_err());
        assert!(corrupt_key.is_some());
    }
}
