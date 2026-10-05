//! Typed source relations and owner-held lazy source snapshots.

use super::complete_coverage;
use crate::workspace::{TransitionWork, WorkspaceRelationHandle, WorkspaceSnapshot};
pub use backend_compile::{
    Container, DeclarationKind, SourceDeclaration, SourceLanguage, SourceLocation,
};
use backend_compile::{
    DeclarationFacts, Deprecation, Fact, MAX_FACT_TEXT_BYTES, Obligation, SourceExcerpt,
    SourceExcerptExtent,
};
use backend_execution::AuthorityVersion;
use backend_library::{
    CargoPackageAliasCargoFactsV1, CargoPackageAliasCoverageV1, CargoPackageAliasEvidenceV1,
    CargoPackageAliasObservationV1, CargoPackageAliasUnavailableV1, CargoPackageAliasV1,
    MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS, MAX_CARGO_PACKAGE_ALIASES,
    MAX_RUST_CARGO_METADATA_TEXT_BYTES,
};
use backend_replication::ImmutableObjectSchema;
use backend_semantic::vocabulary::LanguageProfile;
use backend_version::{
    CanonicalRelation, ContentId, CoverageWitness, CutPolicy, DEFAULT_CUT_POLICY, Relation,
    RelationState, SourceFactDomain, StateRoot, WorkspaceRoot, anchored_cut_points,
};
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;

/// Canonical format tag for a record that states no containment.
const SOURCE_RECORD_FORMAT_PLAIN: &[u8; 4] = b"PSR8";

/// Canonical format tag for a record that carries declaration containment.
const SOURCE_RECORD_FORMAT_CONTAINED: &[u8; 4] = b"PSR9";

/// Canonical format tag for a record that also states its semantic source
/// content identity.
const SOURCE_RECORD_FORMAT_IDENTIFIED: &[u8; 4] = b"PSRA";

/// Canonical format tag for a record in which at least one declaration
/// states what its producer observed about it (deprecation, obligation).
/// It carries containment for every declaration and states its source
/// content identity with a presence byte, because the format discriminant is
/// spent on the facts.
const SOURCE_RECORD_FORMAT_FACTS: &[u8; 4] = b"PSRB";

/// Canonical format tag for a project record carrying source-bound Cargo aliases.
const SOURCE_RECORD_FORMAT_CARGO_ALIASES: &[u8; 4] = b"PSRC";

/// Versioned format carrying project membership page references or one
/// independently checked membership page.
const SOURCE_RECORD_FORMAT_MEMBERSHIP: &[u8; 4] = b"PSRD";

/// Newest source-record format version, as the tags above name it.
const SOURCE_RECORD_VERSION: u8 = 13;

/// Minimum file keys in every non-final membership page. The shared
/// content-defined cut policy also cuts on encoded-byte anchors; 256 remains
/// below its 410-key byte-pressure floor for 32-byte relation keys while
/// bounding the number of page references in a project row.
const MIN_PROJECT_MEMBERSHIP_PAGE_FILES: usize = 256;

/// Maximum file keys in one membership page. The row-value capacity, rather
/// than a logical project limit, remains the hard encoded-byte bound.
const MAX_PROJECT_MEMBERSHIP_PAGE_FILES: usize = 1024;

/// Content-defined cuts use stable file-key anchors between the minimum and
/// maximum page geometry. The forced maximum can reflow later pages in the
/// pathological case where no natural anchor is shared after an edit.
const PROJECT_MEMBERSHIP_CUT_POLICY: CutPolicy = CutPolicy::new(
    MIN_PROJECT_MEMBERSHIP_PAGE_FILES as u16,
    768,
    MAX_PROJECT_MEMBERSHIP_PAGE_FILES as u16,
);

/// Returns the lowest format tag that can carry this record.
///
/// A persisted intent embeds these exact bytes, and restart admission
/// decodes and re-encodes them and refuses anything not byte-identical. A
/// record with nothing new to say must therefore still encode exactly the
/// way it always did, or every workspace written before containment existed
/// would stop opening. The tag is minimal for that reason, the way a
/// canonical integer takes the shortest form: `PSR9` means precisely "at
/// least one declaration states where it sits, `PSRA` means "the row also
/// states which `SourceFactDomain` content identity its exact bytes hash to",
/// `PSRC` carries Cargo alias observations on an inline project frontier, and
/// `PSRD` carries paged project membership or one project-bound membership
/// page.
///
/// Decoding stays liberal - it admits a `PSR9` record that states no
/// containment and normalizes it to `PSR8` on the way out - because refusing
/// a record whose bytes are perfectly readable would make a whole workspace
/// unopenable over a tag.
fn source_record_format(value: &ProductSourceRecord) -> &'static [u8; 4] {
    match value {
        ProductSourceRecord::Project {
            cargo_aliases: Some(_),
            files: ProductProjectMembership::Inline(_),
            ..
        } => SOURCE_RECORD_FORMAT_CARGO_ALIASES,
        ProductSourceRecord::Project {
            files: ProductProjectMembership::Paged { .. },
            ..
        }
        | ProductSourceRecord::MembershipPage { .. } => SOURCE_RECORD_FORMAT_MEMBERSHIP,
        ProductSourceRecord::Project { .. } => SOURCE_RECORD_FORMAT_PLAIN,
        ProductSourceRecord::File {
            declarations,
            source_identity,
            ..
        } => {
            if states_facts(declarations) {
                SOURCE_RECORD_FORMAT_FACTS
            } else if source_identity.is_some() {
                SOURCE_RECORD_FORMAT_IDENTIFIED
            } else if states_containment(declarations) {
                SOURCE_RECORD_FORMAT_CONTAINED
            } else {
                SOURCE_RECORD_FORMAT_PLAIN
            }
        }
    }
}

/// Returns whether any declaration states an observed fact.
fn states_facts(declarations: &[SourceDeclaration]) -> bool {
    declarations
        .iter()
        .any(|declaration| !declaration.facts().is_unobserved())
}

/// Returns whether any declaration sits anywhere but its file module.
fn states_containment(declarations: &[SourceDeclaration]) -> bool {
    declarations
        .iter()
        .any(|declaration| !matches!(declaration.container(), Container::Module))
}

/// Returns the version of an admitted `PSR*` format tag.
fn format_version(format: &[u8]) -> Option<u8> {
    let [b'P', b'S', b'R', tag] = format else {
        return None;
    };
    // `PSRA` is the identified format, `PSRB` adds declaration facts, `PSRC`
    // adds source-bound Cargo aliases, and `PSRD` adds paged project membership.
    let version = match tag {
        b'A' => 10,
        b'B' => 11,
        b'C' => 12,
        b'D' => 13,
        digit if digit.is_ascii_digit() => digit.checked_sub(b'0')?,
        _ => return None,
    };
    (2..=SOURCE_RECORD_VERSION)
        .contains(&version)
        .then_some(version)
}

/// Authoritative package/source coordinates retained by the product
/// workspace. Product input identities are always rooted in this relation;
/// the echo fixture's numeric relation is kept separate above.
#[derive(Debug)]
pub struct ProductSourceRelation;

/// Canonical project or file metadata retained by the product source relation.
///
/// A project record selects an exact ordered file-key frontier, stored inline
/// for historical/small rows or through bounded content-derived pages for
/// larger rows. Each file remains an independent relation value, so changing
/// one file does not rewrite other files or their declaration payloads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductSourceRecord {
    /// One indexed project and the exact set of file rows it selects.
    Project {
        /// Canonical user-facing project coordinate.
        label: String,
        /// Digest over the sorted file identities and content versions.
        source_version: [u8; 32],
        /// Inline legacy file keys or the bounded references to membership
        /// pages selected by this project row.
        files: ProductProjectMembership,
        /// Exact bounded Cargo aliases joined to the current source observations.
        /// `None` preserves historical project-row encoding.
        cargo_aliases: Option<CargoPackageAliasEvidenceV1>,
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
        /// The `SourceFactDomain` content identity the semantic compiler
        /// derives from the same bytes, when it is known to the producer.
        ///
        /// Semantic images commit this identity in their provenance, so the
        /// view can compare content against content: an in-place edit under
        /// an unchanged path set is visible as an identity mismatch, which
        /// the `ObjectVersion`-domain `content_version` above cannot prove.
        source_identity: Option<ContentId<SourceFactDomain>>,
        /// Ordered declarations extracted from this file.
        declarations: Arc<[SourceDeclaration]>,
        /// How much extracted detail this row was able to retain.
        retention: DeclarationRetention,
    },
    /// A bounded, project-bound run of sorted source-file relation keys.
    ///
    /// The page key commits these exact bytes; the project row lists page
    /// keys in file-key order and carries the total file count.
    MembershipPage {
        /// Project identity that owns every file key in this page.
        project: [u8; 32],
        /// Strictly increasing source-file relation keys in this page.
        files: Arc<[[u8; 32]]>,
    },
}

/// Logical membership representation carried by a project record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductProjectMembership {
    /// Historical project rows store the complete frontier inline.
    Inline(Arc<[[u8; 32]]>),
    /// New large projects name bounded content-derived membership pages.
    Paged {
        /// Exact number of file keys selected across all referenced pages.
        file_count: u32,
        /// Membership page relation keys in file-key range order.
        page_keys: Arc<[[u8; 32]]>,
    },
}

/// Membership form exposed by a borrowed project record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductProjectFileMembership<'a> {
    /// Complete sorted file-key frontier retained by a historical inline row.
    Inline(&'a [[u8; 32]]),
    /// Exact file count and ordered content-derived membership page keys.
    Paged {
        /// Number of files selected by the project.
        file_count: usize,
        /// Page row keys in the same order as their file-key ranges.
        page_keys: &'a [[u8; 32]],
    },
}

impl<'a> ProductProjectFileMembership<'a> {
    /// Returns the selected file count recorded by this membership form.
    #[must_use]
    pub fn file_count(self) -> usize {
        match self {
            Self::Inline(files) => files.len(),
            Self::Paged { file_count, .. } => file_count,
        }
    }

    /// Returns page row keys, or an empty slice for an inline legacy row.
    #[must_use]
    pub fn page_keys(self) -> &'a [[u8; 32]] {
        match self {
            Self::Inline(_) => &[],
            Self::Paged { page_keys, .. } => page_keys,
        }
    }
}

/// One project-row update together with any new immutable membership pages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductSourceProjectUpdate {
    project_key: [u8; 32],
    project: ProductSourceRecord,
    pages: Box<[([u8; 32], ProductSourceRecord)]>,
}

impl ProductSourceProjectUpdate {
    /// Returns the relation key for the project's canonical package label.
    #[must_use]
    pub const fn project_key(&self) -> [u8; 32] {
        self.project_key
    }

    /// Returns the project row containing its exact count and page references.
    #[must_use]
    pub const fn project_record(&self) -> &ProductSourceRecord {
        &self.project
    }

    /// Returns newly constructed membership page rows in frontier order.
    #[must_use]
    pub fn membership_pages(&self) -> &[([u8; 32], ProductSourceRecord)] {
        &self.pages
    }
}

/// Number of extracted declarations a truncated file row still retains.
///
/// Both counts are authoritative evidence rather than display hints: a
/// surface that renders `retained` without `extracted` would present a
/// partial outline as a complete one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedDeclarations {
    retained: u32,
    extracted: u32,
}

impl RetainedDeclarations {
    /// Constructs a checked retention count.
    ///
    /// # Errors
    /// Returns an error when more declarations are retained than extracted.
    pub fn new(retained: u32, extracted: u32) -> Result<Self, String> {
        if retained > extracted {
            return Err("retained declaration count exceeds the extracted count".to_owned());
        }
        Ok(Self {
            retained,
            extracted,
        })
    }

    /// Returns how many declarations the row retains.
    #[must_use]
    pub const fn retained(self) -> u32 {
        self.retained
    }

    /// Returns how many declarations the frontend extracted.
    #[must_use]
    pub const fn extracted(self) -> u32 {
        self.extracted
    }
}

impl fmt::Display for RetainedDeclarations {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.retained, self.extracted)
    }
}

/// Why a source file in the project frontier has no extracted detail.
///
/// A project must survive the files it contains.  One unreadable, binary, or
/// oversized file used to abort the whole scan, so a single bad byte in a
/// large checkout made the project unindexable.  The file instead keeps its
/// place in the frontier and states, in typed form, what could not be known
/// about it.
///
/// The vocabulary itself lives in the portable product contract rather than
/// here. It is written into a canonical relation row *and* rendered by every
/// surface, and two copies of a closed four-variant terminal set are two
/// copies that can drift: a fifth reason added on one side is a silent gap on
/// the other. The relation owns the *encoding* of these discriminants; the
/// product contract owns the set.
pub use backend_library::SourceUnavailableReason;

/// How much of a source file's extracted detail one relation row retains.
///
/// One canonical relation row can never exceed
/// [`ProductSourceRecord::ROW_VALUE_CAPACITY`] encoded bytes, while a single
/// real source file can extract far more than that.  Rather than rejecting
/// the file - which used to fail the whole project with an opaque
/// `workspace relation node was rejected` - the producer sheds derived detail
/// in a fixed order and records exactly how far it had to go.  Every surface
/// can therefore state what is missing instead of presenting a reduced row as
/// a complete one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeclarationRetention {
    /// Every extracted declaration is retained with its full text.
    #[default]
    Complete,
    /// Source excerpts were replaced by [`SourceExcerpt::NotHydrated`].
    ExcerptsElided,
    /// Excerpts, documentation, and signatures were dropped; names, kinds,
    /// and locations remain authoritative.
    NamesOnly,
    /// Only a leading run of the extracted declarations is retained.
    Truncated(RetainedDeclarations),
    /// Nothing could be extracted from this file, for the stated reason.
    Unavailable(SourceUnavailableReason),
}

impl DeclarationRetention {
    /// Returns whether this row carries every extracted declaration field.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

impl fmt::Display for DeclarationRetention {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Complete => formatter.write_str("complete"),
            Self::ExcerptsElided => formatter.write_str("excerpts elided"),
            Self::NamesOnly => formatter.write_str("names only"),
            Self::Truncated(counts) => write!(formatter, "truncated {counts}"),
            Self::Unavailable(reason) => write!(formatter, "unavailable ({reason})"),
        }
    }
}

/// Borrowed project frontier exposed without tuple-position coupling.
#[derive(Clone, Copy, Debug)]
pub struct ProductProjectRef<'a> {
    /// Canonical user-facing project coordinate.
    pub label: &'a str,
    /// Digest over the selected file identities and content versions.
    pub source_version: [u8; 32],
    /// Inline file keys or the exact page references selected by the project.
    pub files: ProductProjectFileMembership<'a>,
    /// Exact Cargo aliases with their per-profile source bindings.
    pub cargo_aliases: Option<&'a CargoPackageAliasEvidenceV1>,
}

/// Borrowed fields from one bounded project-membership page.
#[derive(Clone, Copy, Debug)]
pub struct ProductSourceMembershipPageRef<'a> {
    /// Project relation key that owns the page.
    pub project: [u8; 32],
    /// Strictly increasing file relation keys retained by this page.
    pub files: &'a [[u8; 32]],
}

impl<'a> ProductProjectRef<'a> {
    /// Streams the complete ordered file-key frontier, resolving one
    /// membership page at a time through the selected relation root.
    ///
    /// Callers that publish or render a project must consume the iterator to
    /// completion and propagate any error before exposing rows; a later
    /// missing or corrupt page invalidates the whole frontier.
    pub fn iter_file_keys<'lookup, Lookup>(
        self,
        project: [u8; 32],
        lookup: Lookup,
    ) -> impl Iterator<Item = Result<[u8; 32], String>> + 'lookup
    where
        Lookup: FnMut(&[u8; 32]) -> Result<Option<ProductSourceRecord>, String> + 'lookup,
        'a: 'lookup,
    {
        ProjectFileKeyIterator::new(project, self.files, lookup)
    }
}

struct ProjectFileKeyIterator<'a, Lookup> {
    project: [u8; 32],
    membership: ProductProjectFileMembership<'a>,
    lookup: Lookup,
    inline_index: usize,
    page_index: usize,
    page_file_index: usize,
    page: Option<([u8; 32], ProductSourceRecord)>,
    previous: Option<[u8; 32]>,
    seen: usize,
    first_error: Option<String>,
    failed: bool,
    finished: bool,
}

impl<'a, Lookup> ProjectFileKeyIterator<'a, Lookup>
where
    Lookup: FnMut(&[u8; 32]) -> Result<Option<ProductSourceRecord>, String>,
{
    fn new(
        project: [u8; 32],
        membership: ProductProjectFileMembership<'a>,
        lookup: Lookup,
    ) -> Self {
        let first_error = match membership {
            ProductProjectFileMembership::Inline(files)
                if files.len() > ProductSourceRecord::MAX_PROJECT_FILES
                    || files.windows(2).any(|window| window[0] >= window[1]) =>
            {
                Some("inline project membership is unordered or oversized".to_owned())
            }
            ProductProjectFileMembership::Paged {
                file_count,
                page_keys,
            } => validate_paged_membership(file_count, page_keys).err(),
            ProductProjectFileMembership::Inline(_) => None,
        };
        Self {
            project,
            membership,
            lookup,
            inline_index: 0,
            page_index: 0,
            page_file_index: 0,
            page: None,
            previous: None,
            seen: 0,
            first_error,
            failed: false,
            finished: false,
        }
    }

    fn next_key(&mut self) -> Option<Result<[u8; 32], String>> {
        if self.finished || self.failed {
            return None;
        }
        if let Some(error) = self.first_error.take() {
            self.failed = true;
            return Some(Err(error));
        }

        loop {
            let (expected_count, key) = match self.membership {
                ProductProjectFileMembership::Inline(files) => {
                    let Some(key) = files.get(self.inline_index).copied() else {
                        self.finished = true;
                        return None;
                    };
                    self.inline_index += 1;
                    (files.len(), key)
                }
                ProductProjectFileMembership::Paged {
                    file_count,
                    page_keys,
                } => {
                    if let Some((_, page)) = &self.page {
                        let Some(fields) = page.membership_page_fields() else {
                            self.failed = true;
                            return Some(Err(
                                "membership reference resolves to a non-page row".into()
                            ));
                        };
                        if let Some(key) = fields.files.get(self.page_file_index).copied() {
                            self.page_file_index += 1;
                            (file_count, key)
                        } else {
                            self.page = None;
                            self.page_file_index = 0;
                            continue;
                        }
                    } else if let Some(page_key) = page_keys.get(self.page_index).copied() {
                        self.page_index += 1;
                        let record = match (self.lookup)(&page_key) {
                            Ok(Some(record)) => record,
                            Ok(None) => {
                                self.failed = true;
                                return Some(Err("project membership page is missing".into()));
                            }
                            Err(error) => {
                                self.failed = true;
                                return Some(Err(format!("read project membership page: {error}")));
                            }
                        };
                        let Some(fields) = record.membership_page_fields() else {
                            self.failed = true;
                            return Some(Err(
                                "membership reference resolves to a non-page row".into()
                            ));
                        };
                        if fields.project != self.project {
                            self.failed = true;
                            return Some(Err(
                                "project membership page belongs to another project".into()
                            ));
                        }
                        let page_key_matches =
                            product_source_membership_page_key(self.project, fields.files)
                                .is_ok_and(|expected| expected == page_key);
                        if !page_key_matches {
                            self.failed = true;
                            return Some(Err(
                                "project membership page identity does not match its contents"
                                    .into(),
                            ));
                        }
                        if self.page_index < page_keys.len()
                            && fields.files.len() < MIN_PROJECT_MEMBERSHIP_PAGE_FILES
                        {
                            self.failed = true;
                            return Some(Err(
                                "non-final project membership page is below its minimum size"
                                    .into(),
                            ));
                        }
                        self.page = Some((page_key, record));
                        continue;
                    } else {
                        if self.seen != file_count {
                            self.failed = true;
                            return Some(Err(
                                "project membership page count does not match its file count"
                                    .into(),
                            ));
                        }
                        self.finished = true;
                        return None;
                    }
                }
            };
            if self.previous.is_some_and(|previous| previous >= key) {
                self.failed = true;
                return Some(Err(
                    "project membership pages are unordered or duplicate a file key".into(),
                ));
            }
            self.previous = Some(key);
            self.seen = match self.seen.checked_add(1) {
                Some(seen) if seen <= expected_count => seen,
                _ => {
                    self.failed = true;
                    return Some(Err(
                        "project membership exceeds its declared file count".into()
                    ));
                }
            };
            if self.seen == expected_count
                && matches!(self.membership, ProductProjectFileMembership::Inline(_))
            {
                self.finished = true;
            }
            return Some(Ok(key));
        }
    }
}

impl<Lookup> Iterator for ProjectFileKeyIterator<'_, Lookup>
where
    Lookup: FnMut(&[u8; 32]) -> Result<Option<ProductSourceRecord>, String>,
{
    type Item = Result<[u8; 32], String>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_key()
    }
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
    /// The semantic source content identity, when the producer stated it.
    pub source_identity: Option<ContentId<SourceFactDomain>>,
    /// Shared declaration storage.
    pub declarations: &'a Arc<[SourceDeclaration]>,
    /// How much extracted detail this row was able to retain.
    pub retention: DeclarationRetention,
}

impl ProductSourceRecord {
    /// Maximum admitted coordinate or relative-path length.
    pub const MAX_LABEL_BYTES: usize = 4096;
    /// Maximum source files selected by one project across its membership.
    pub const MAX_PROJECT_FILES: usize = backend_library::MAX_SELECTED_PROJECT_FRONTIER_FILES;
    /// Maximum declarations retained in one source file.
    pub const MAX_FILE_DECLARATIONS: usize = 16_384;
    /// Largest encoded value one row of this relation may carry.
    ///
    /// The canonical tree admits no node above
    /// [`backend_version::CutPolicy::MAX_ENCODED_BYTES`], and a leaf holding
    /// one entry spends the node header plus a length-delimited key and value
    /// field.  Every constructor below is checked against this bound, because
    /// a record that exceeds it has no representable row: the failure would
    /// otherwise surface only when the relation delta was prepared, as an
    /// opaque rejection naming neither the file nor the reason.
    pub const ROW_VALUE_CAPACITY: usize = match DEFAULT_CUT_POLICY
        .max_row_value_bytes(size_of::<<ProductSourceRelation as Relation>::Key>())
    {
        Some(capacity) => capacity,
        None => 0,
    };
    /// Largest legacy inline frontier that fits beside a maximum-length
    /// project label without Cargo aliases. New project updates also compare
    /// actual encoded size, so aliases can move a smaller frontier to pages.
    pub const MAX_FRONTIER_FILES: usize =
        (Self::ROW_VALUE_CAPACITY - Self::MAX_LABEL_BYTES - 64) / 32;
    /// Largest admitted number of page references in one project row. With a
    /// 100,000-file logical frontier and 256 keys in every non-final page, at
    /// most 391 references are needed (12,512 encoded key bytes), before the
    /// bounded label and optional alias evidence.
    pub const MAX_PROJECT_MEMBERSHIP_PAGES: usize =
        (Self::MAX_PROJECT_FILES + MIN_PROJECT_MEMBERSHIP_PAGE_FILES - 1)
            / MIN_PROJECT_MEMBERSHIP_PAGE_FILES;

    fn encoded_value_bytes(&self) -> usize {
        let mut bytes = Vec::new();
        ProductSourceRelation::encode_value(self, &mut bytes);
        bytes.len()
    }

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
        Self::project_with_aliases(label, source_version, files, None)
    }

    /// Constructs a project frontier with exact source-bound Cargo aliases.
    /// If the aliases alone push an otherwise valid project row over its
    /// canonical byte capacity, the record retains each observation and
    /// reports `ProjectRowCapacity` instead of silently omitting them.
    pub fn project_with_cargo_aliases(
        label: impl Into<String>,
        source_version: [u8; 32],
        files: impl Into<Vec<[u8; 32]>>,
        cargo_aliases: CargoPackageAliasEvidenceV1,
    ) -> Result<Self, String> {
        cargo_aliases.admit().map_err(|_| {
            "Cargo package alias evidence is malformed or exceeds its bounds".to_owned()
        })?;
        Self::project_with_aliases(label, source_version, files, Some(cargo_aliases))
    }

    fn project_with_aliases(
        label: impl Into<String>,
        source_version: [u8; 32],
        files: impl Into<Vec<[u8; 32]>>,
        cargo_aliases: Option<CargoPackageAliasEvidenceV1>,
    ) -> Result<Self, String> {
        let label = label.into();
        let files = files.into();
        if label.is_empty() || label.len() > Self::MAX_LABEL_BYTES {
            return Err("project source coordinate is empty or oversized".to_owned());
        }
        if files.windows(2).any(|window| window[0] >= window[1]) {
            return Err("project file frontier is unordered or holds a duplicate".to_owned());
        }
        if files.len() > Self::MAX_PROJECT_FILES {
            return Err(format!(
                "project file frontier of {} files exceeds the {} file logical limit",
                files.len(),
                Self::MAX_PROJECT_FILES
            ));
        }
        let record = Self::Project {
            label,
            source_version,
            files: ProductProjectMembership::Inline(Arc::from(files.into_boxed_slice())),
            cargo_aliases,
        };
        let mut record = record;
        let mut encoded = record.encoded_value_bytes();
        if encoded > Self::ROW_VALUE_CAPACITY
            && let Self::Project {
                cargo_aliases: Some(evidence),
                ..
            } = &mut record
        {
            *evidence = evidence.unavailable_for_project_row_capacity();
            encoded = record.encoded_value_bytes();
        }
        if encoded > Self::ROW_VALUE_CAPACITY {
            let named = record
                .project_fields()
                .map_or(0, |fields| fields.files.file_count());
            let overhead = encoded.saturating_sub(named.saturating_mul(32));
            let admitted = Self::ROW_VALUE_CAPACITY.saturating_sub(overhead) / 32;
            return Err(format!(
                "project row of {encoded} bytes exceeds the {} byte canonical row capacity; \
                 this coordinate admits at most {admitted} files",
                Self::ROW_VALUE_CAPACITY
            ));
        }
        Ok(record)
    }

    /// Constructs a project-row update with bounded immutable membership
    /// pages whenever the complete inline frontier would exceed its stable
    /// threshold or the canonical row byte capacity.
    ///
    /// The page planner uses the relation's existing content-defined key
    /// cuts with a 256/768/1024 file geometry. Its minimum remains below the
    /// shared cut planner's first byte-anchor boundary (410 keys for 32-byte
    /// relation keys). Page identities commit the full owning project key
    /// and every file key in that page, so unchanged
    /// pages retain their identities across edits. A forced maximum cut can
    /// reflow following pages when no later natural anchor is shared.
    ///
    /// # Errors
    /// Returns an error for an invalid project label, unordered or duplicate
    /// file keys, more than [`Self::MAX_PROJECT_FILES`] files, malformed alias
    /// evidence, or a project row/page that exceeds its canonical bounds.
    pub fn project_with_membership_pages(
        label: impl Into<String>,
        source_version: [u8; 32],
        files: impl Into<Vec<[u8; 32]>>,
        cargo_aliases: Option<CargoPackageAliasEvidenceV1>,
    ) -> Result<ProductSourceProjectUpdate, String> {
        let label = label.into();
        let files = files.into();
        validate_project_frontier(&label, &files)?;
        if let Some(aliases) = &cargo_aliases {
            aliases.admit().map_err(|_| {
                "Cargo package alias evidence is malformed or exceeds its bounds".to_owned()
            })?;
        }

        let project_key = backend_library::package_key(&label).to_bytes();
        if files.len() <= Self::MAX_FRONTIER_FILES {
            let project = Self::Project {
                label: label.clone(),
                source_version,
                files: ProductProjectMembership::Inline(Arc::from(
                    files.clone().into_boxed_slice(),
                )),
                cargo_aliases: cargo_aliases.clone(),
            };
            if project.encoded_value_bytes() <= Self::ROW_VALUE_CAPACITY {
                return Ok(ProductSourceProjectUpdate {
                    project_key,
                    project,
                    pages: Box::new([]),
                });
            }
            if files.is_empty() {
                // Empty frontiers have nothing to page; preserve the legacy
                // typed unavailable marker if optional aliases alone overflow.
                let project =
                    Self::project_with_aliases(label, source_version, files, cargo_aliases)?;
                return Ok(ProductSourceProjectUpdate {
                    project_key,
                    project,
                    pages: Box::new([]),
                });
            }
        }

        let cuts =
            anchored_cut_points::<ProductSourceRelation>(&files, PROJECT_MEMBERSHIP_CUT_POLICY, 0)
                .map_err(|error| format!("project membership page cuts are invalid: {error}"))?;
        let mut page_keys = Vec::with_capacity(cuts.len());
        let mut pages = Vec::with_capacity(cuts.len());
        let mut start = 0usize;
        for end in cuts {
            let page_files = files
                .get(start..end)
                .ok_or_else(|| "project membership page cut is outside its frontier".to_owned())?;
            let page = Self::membership_page(project_key, page_files.to_vec())?;
            let page_key = product_source_membership_page_key(project_key, page_files)?;
            if page_key == project_key
                || files.binary_search(&page_key).is_ok()
                || page_keys.contains(&page_key)
            {
                return Err("project membership page key collides with a selected row".to_owned());
            }
            page_keys.push(page_key);
            pages.push((page_key, page));
            start = end;
        }
        if start != files.len() {
            return Err("project membership cuts did not cover the complete frontier".to_owned());
        }
        let project = Self::paged_project_with_aliases(
            label,
            source_version,
            files.len(),
            page_keys,
            cargo_aliases,
        )?;
        Ok(ProductSourceProjectUpdate {
            project_key,
            project,
            pages: pages.into_boxed_slice(),
        })
    }

    fn paged_project_with_aliases(
        label: String,
        source_version: [u8; 32],
        file_count: usize,
        page_keys: Vec<[u8; 32]>,
        cargo_aliases: Option<CargoPackageAliasEvidenceV1>,
    ) -> Result<Self, String> {
        if label.is_empty() || label.len() > ProductSourceRecord::MAX_LABEL_BYTES {
            return Err("project source coordinate is empty or oversized".to_owned());
        }
        validate_paged_membership(file_count, &page_keys)?;
        if let Some(aliases) = &cargo_aliases {
            aliases.admit().map_err(|_| {
                "Cargo package alias evidence is malformed or exceeds its bounds".to_owned()
            })?;
        }
        let mut record = Self::Project {
            label,
            source_version,
            files: ProductProjectMembership::Paged {
                file_count: u32::try_from(file_count)
                    .map_err(|_| "project file count exceeds u32".to_owned())?,
                page_keys: Arc::from(page_keys.into_boxed_slice()),
            },
            cargo_aliases,
        };
        let mut encoded = record.encoded_value_bytes();
        if encoded > Self::ROW_VALUE_CAPACITY
            && let Self::Project {
                cargo_aliases: Some(evidence),
                ..
            } = &mut record
        {
            *evidence = evidence.unavailable_for_project_row_capacity();
            encoded = record.encoded_value_bytes();
        }
        if encoded > Self::ROW_VALUE_CAPACITY {
            return Err(format!(
                "project membership index is {encoded} bytes, above the {} byte canonical row capacity",
                Self::ROW_VALUE_CAPACITY
            ));
        }
        Ok(record)
    }

    fn membership_page(project: [u8; 32], files: Vec<[u8; 32]>) -> Result<Self, String> {
        if files.is_empty()
            || files.len() > MAX_PROJECT_MEMBERSHIP_PAGE_FILES
            || files.windows(2).any(|window| window[0] >= window[1])
        {
            return Err("project membership page is empty, unordered, or oversized".to_owned());
        }
        let page = Self::MembershipPage {
            project,
            files: Arc::from(files.into_boxed_slice()),
        };
        let encoded = page.encoded_value_bytes();
        if encoded > Self::ROW_VALUE_CAPACITY {
            return Err(format!(
                "project membership page is {encoded} bytes, above the {} byte canonical row capacity",
                Self::ROW_VALUE_CAPACITY
            ));
        }
        Ok(page)
    }

    /// Constructs a checked file record that retains every declaration.
    ///
    /// # Errors
    /// Returns an error for an invalid path, an oversized declaration set, or
    /// a row that cannot fit [`Self::ROW_VALUE_CAPACITY`].  Producers that
    /// scan real source should call [`Self::file_within_row_capacity`], which
    /// sheds derived detail instead of failing.
    pub fn file(
        project: [u8; 32],
        path: impl Into<String>,
        language: SourceLanguage,
        content_version: [u8; 32],
        analysis_version: [u8; 32],
        declarations: impl Into<Arc<[SourceDeclaration]>>,
    ) -> Result<Self, String> {
        Self::file_with_retention(
            project,
            path,
            language,
            content_version,
            analysis_version,
            declarations,
            DeclarationRetention::Complete,
        )
    }

    /// Constructs a checked file record carrying an explicit retention claim.
    ///
    /// # Errors
    /// Returns an error for an invalid path, an oversized declaration set, or
    /// a row above [`Self::ROW_VALUE_CAPACITY`].
    pub fn file_with_retention(
        project: [u8; 32],
        path: impl Into<String>,
        language: SourceLanguage,
        content_version: [u8; 32],
        analysis_version: [u8; 32],
        declarations: impl Into<Arc<[SourceDeclaration]>>,
        retention: DeclarationRetention,
    ) -> Result<Self, String> {
        let path = path.into();
        let declarations = declarations.into();
        if path.is_empty() || path.len() > Self::MAX_LABEL_BYTES {
            return Err("source file path is empty or oversized".to_owned());
        }
        if declarations.len() > Self::MAX_FILE_DECLARATIONS {
            return Err("source file declaration set is oversized".to_owned());
        }
        let record = Self::File {
            project,
            path,
            language,
            content_version,
            analysis_version,
            source_identity: None,
            declarations,
            retention,
        };
        let encoded = record.encoded_value_bytes();
        if encoded > Self::ROW_VALUE_CAPACITY {
            return Err(format!(
                "source file row for {} is {encoded} bytes, above the {} byte canonical row capacity",
                record.label(),
                Self::ROW_VALUE_CAPACITY
            ));
        }
        Ok(record)
    }

    /// Constructs a file record for a source the producer could not extract.
    ///
    /// The row still names the file so the project frontier stays complete and
    /// a surface can list it, but it retains no declarations and states why.
    /// The content version is zero because nothing about the bytes is known:
    /// a later scan that succeeds therefore changes the row.
    ///
    /// # Errors
    /// Returns an error for an invalid path.
    pub fn file_unavailable(
        project: [u8; 32],
        path: impl Into<String>,
        language: SourceLanguage,
        analysis_version: [u8; 32],
        reason: SourceUnavailableReason,
    ) -> Result<Self, String> {
        Self::file_with_retention(
            project,
            path,
            language,
            [0; 32],
            analysis_version,
            Vec::new(),
            DeclarationRetention::Unavailable(reason),
        )
    }

    /// Constructs a file record that always fits one canonical relation row.
    ///
    /// A single real source file routinely extracts more detail than one row
    /// can carry; `memchr 2.8.3` alone produces a 65 686 byte row for
    /// `src/arch/x86_64/avx2/memchr.rs`.  Detail is therefore shed in a fixed,
    /// content-addressed order - excerpts, then prose and signatures, then a
    /// trailing run of declarations - and the resulting
    /// [`DeclarationRetention`] states exactly which step was needed.  The
    /// result is a pure function of the inputs, so an unchanged file yields an
    /// unchanged row.
    ///
    /// # Errors
    /// Returns an error for an invalid path, an oversized declaration set, or
    /// a file whose bare identity cannot fit a row at all.
    pub fn file_within_row_capacity(
        project: [u8; 32],
        path: impl Into<String>,
        language: SourceLanguage,
        content_version: [u8; 32],
        analysis_version: [u8; 32],
        declarations: impl Into<Arc<[SourceDeclaration]>>,
    ) -> Result<Self, String> {
        let path = path.into();
        let extracted = declarations.into();
        let build = |declarations: Arc<[SourceDeclaration]>, retention| {
            Self::file_with_retention(
                project,
                path.clone(),
                language,
                content_version,
                analysis_version,
                declarations,
                retention,
            )
        };
        if let Ok(record) = build(Arc::clone(&extracted), DeclarationRetention::Complete) {
            return Ok(record);
        }
        let without_excerpts = shed_excerpts(&extracted);
        if let Ok(record) = build(
            Arc::clone(&without_excerpts),
            DeclarationRetention::ExcerptsElided,
        ) {
            return Ok(record);
        }
        let names_only = shed_prose(&extracted)?;
        if let Ok(record) = build(Arc::clone(&names_only), DeclarationRetention::NamesOnly) {
            return Ok(record);
        }
        // The prefix search must weigh the retention the record will really
        // carry. Probing with `NamesOnly` and then writing `Truncated` picks a
        // prefix that fits without the two retained/extracted counts and then
        // adds them, so the largest prefix overshoots the node by exactly
        // those bytes and the whole shedding ladder ends in the rejection it
        // exists to avoid. `RetainedDeclarations` is fixed width, so the
        // encoded size of a candidate depends only on its length and the
        // search stays monotone.
        let extracted_count =
            u32::try_from(extracted.len()).map_err(|_| "extracted declaration count overflow")?;
        let truncated_retention = |length: usize| {
            u32::try_from(length)
                .ok()
                .and_then(|retained| RetainedDeclarations::new(retained, extracted_count).ok())
                .map(DeclarationRetention::Truncated)
        };
        let retained = largest_fitting_prefix(&names_only, |prefix| {
            truncated_retention(prefix.len())
                .is_some_and(|retention| build(prefix, retention).is_ok())
        });
        let counts = RetainedDeclarations::new(
            u32::try_from(retained).map_err(|_| "retained declaration count overflow")?,
            extracted_count,
        )?;
        build(
            names_only
                .get(..retained)
                .ok_or("retained declaration prefix is out of range")?
                .into(),
            DeclarationRetention::Truncated(counts),
        )
    }

    /// Returns the display coordinate for this project or file.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Project { label, .. } => label,
            Self::File { path, .. } => path,
            Self::MembershipPage { .. } => "project membership page",
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
                cargo_aliases,
            } => Some(ProductProjectRef {
                label,
                source_version: *source_version,
                files: match files {
                    ProductProjectMembership::Inline(files) => {
                        ProductProjectFileMembership::Inline(files)
                    }
                    ProductProjectMembership::Paged {
                        file_count,
                        page_keys,
                    } => ProductProjectFileMembership::Paged {
                        file_count: *file_count as usize,
                        page_keys,
                    },
                },
                cargo_aliases: cargo_aliases.as_ref(),
            }),
            Self::File { .. } | Self::MembershipPage { .. } => None,
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
                source_identity,
                declarations,
                retention,
            } => Some(ProductFileRef {
                project: *project,
                path,
                language: *language,
                content_version: *content_version,
                analysis_version: *analysis_version,
                source_identity: *source_identity,
                declarations,
                retention: *retention,
            }),
            Self::Project { .. } | Self::MembershipPage { .. } => None,
        }
    }

    /// Returns the bounded membership-page fields when this is a page row.
    #[must_use]
    pub fn membership_page_fields(&self) -> Option<ProductSourceMembershipPageRef<'_>> {
        match self {
            Self::MembershipPage { project, files } => Some(ProductSourceMembershipPageRef {
                project: *project,
                files,
            }),
            Self::Project { .. } | Self::File { .. } => None,
        }
    }

    /// Binds the semantic source content identity to this file record.
    ///
    /// The identity is the exact `ContentId<SourceFactDomain>` the semantic
    /// compiler derives from the same source bytes, so a view that persisted
    /// it per file can prove an in-place edit with a content-to-content
    /// comparison instead of a path-set diff.
    ///
    /// # Errors
    ///
    /// Returns an error when this record is a project frontier, which names
    /// no source bytes at all.
    pub fn with_source_identity(
        mut self,
        identity: ContentId<SourceFactDomain>,
    ) -> Result<Self, String> {
        match &mut self {
            Self::File {
                source_identity, ..
            } => {
                *source_identity = Some(identity);
                Ok(self)
            }
            Self::Project { .. } | Self::MembershipPage { .. } => Err(
                "project and membership rows name no source bytes, so they carry no content identity"
                    .to_owned(),
            ),
        }
    }

    /// Rebuilds this project frontier with one exact alias-evidence set.
    ///
    /// # Errors
    /// Returns an error when this is not a project record or the alias
    /// evidence itself is malformed.
    pub fn with_cargo_aliases(
        self,
        cargo_aliases: CargoPackageAliasEvidenceV1,
    ) -> Result<Self, String> {
        match self {
            Self::Project {
                label,
                source_version,
                files,
                ..
            } => {
                cargo_aliases.admit().map_err(|_| {
                    "Cargo package alias evidence is malformed or exceeds its bounds".to_owned()
                })?;
                let record = Self::Project {
                    label,
                    source_version,
                    files,
                    cargo_aliases: Some(cargo_aliases),
                };
                let mut record = record;
                if record.encoded_value_bytes() > Self::ROW_VALUE_CAPACITY
                    && let Self::Project {
                        cargo_aliases: Some(evidence),
                        ..
                    } = &mut record
                {
                    *evidence = evidence.unavailable_for_project_row_capacity();
                }
                if record.encoded_value_bytes() > Self::ROW_VALUE_CAPACITY {
                    return Err(
                        "project membership references exceed the canonical row capacity"
                            .to_owned(),
                    );
                }
                Ok(record)
            }
            Self::File { .. } => {
                Err("a source file row cannot carry project Cargo package aliases".to_owned())
            }
            Self::MembershipPage { .. } => {
                Err("a membership page cannot carry project Cargo package aliases".to_owned())
            }
        }
    }
}

/// Derives one membership-page relation key from the complete page payload.
///
/// # Errors
/// Returns an error when the page is empty, unordered, duplicated, or larger
/// than the canonical per-row membership bound.
pub fn product_source_membership_page_key(
    project: [u8; 32],
    files: &[[u8; 32]],
) -> Result<[u8; 32], String> {
    if files.is_empty()
        || files.len() > MAX_PROJECT_MEMBERSHIP_PAGE_FILES
        || files.windows(2).any(|window| window[0] >= window[1])
    {
        return Err("project membership page is empty, unordered, or oversized".to_owned());
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.product-source.membership-page.v1\0");
    hasher.update(&project);
    hasher.update(
        &u32::try_from(files.len())
            .map_err(|_| "project membership page count exceeds u32".to_owned())?
            .to_be_bytes(),
    );
    for file in files {
        hasher.update(file);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn validate_project_frontier(label: &str, files: &[[u8; 32]]) -> Result<(), String> {
    if label.is_empty() || label.len() > ProductSourceRecord::MAX_LABEL_BYTES {
        return Err("project source coordinate is empty or oversized".to_owned());
    }
    if files.windows(2).any(|window| window[0] >= window[1]) {
        return Err("project file frontier is unordered or holds a duplicate".to_owned());
    }
    if files.len() > ProductSourceRecord::MAX_PROJECT_FILES {
        return Err(format!(
            "project file frontier of {} files exceeds the {} file logical limit",
            files.len(),
            ProductSourceRecord::MAX_PROJECT_FILES
        ));
    }
    Ok(())
}

fn validate_paged_membership(file_count: usize, page_keys: &[[u8; 32]]) -> Result<(), String> {
    if file_count == 0 || file_count > ProductSourceRecord::MAX_PROJECT_FILES {
        return Err("paged project membership has an invalid file count".to_owned());
    }
    let maximum_pages = file_count
        .checked_add(MIN_PROJECT_MEMBERSHIP_PAGE_FILES - 1)
        .ok_or_else(|| "project membership page count overflows".to_owned())?
        / MIN_PROJECT_MEMBERSHIP_PAGE_FILES;
    if page_keys.is_empty()
        || page_keys.len() > maximum_pages
        || page_keys.len() > ProductSourceRecord::MAX_PROJECT_MEMBERSHIP_PAGES
    {
        return Err("project membership page references exceed their count bound".to_owned());
    }
    let mut seen = std::collections::BTreeSet::new();
    if page_keys.iter().any(|key| !seen.insert(*key)) {
        return Err("project membership repeats a page reference".to_owned());
    }
    Ok(())
}

/// Replaces every captured excerpt with the typed not-hydrated marker.
fn shed_excerpts(declarations: &[SourceDeclaration]) -> Arc<[SourceDeclaration]> {
    declarations
        .iter()
        .map(|declaration| {
            declaration
                .clone()
                .with_source_excerpt(SourceExcerpt::NotHydrated)
        })
        .collect::<Vec<_>>()
        .into()
}

/// Keeps only each declaration's name, kind, and location.
fn shed_prose(declarations: &[SourceDeclaration]) -> Result<Arc<[SourceDeclaration]>, String> {
    declarations
        .iter()
        .map(|declaration| {
            SourceDeclaration::with_location(
                declaration.location().clone(),
                declaration.name(),
                declaration.kind().name(),
                "",
                "",
            )
            // Containment is structure, not prose: a field that lost its
            // parent here would be reparented to its file module and the
            // shedding ladder would silently flatten a large file's outline.
            .map(|reduced| {
                reduced
                    .with_source_excerpt(SourceExcerpt::NotHydrated)
                    .with_container(declaration.container().clone())
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Into::into)
}

/// Returns the longest prefix length for which `fits` holds.
///
/// `fits` is monotone in the prefix length because every additional
/// declaration only adds encoded bytes, so a binary search returns the exact
/// boundary while encoding `log2(n)` candidates rather than `n`.
fn largest_fitting_prefix<F>(declarations: &[SourceDeclaration], fits: F) -> usize
where
    F: Fn(Arc<[SourceDeclaration]>) -> bool,
{
    let mut low = 0usize;
    let mut high = declarations.len();
    while low < high {
        let middle = high
            .saturating_sub(low)
            .div_ceil(2)
            .saturating_add(low)
            .min(high);
        let Some(prefix) = declarations.get(..middle) else {
            return low;
        };
        if fits(prefix.into()) {
            low = middle;
        } else {
            high = middle.saturating_sub(1);
        }
    }
    low
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
        output.extend_from_slice(source_record_format(value));
        match value {
            ProductSourceRecord::Project {
                label,
                source_version,
                files,
                cargo_aliases,
            } => {
                output.push(1);
                push_text(output, label);
                output.extend_from_slice(source_version);
                match files {
                    ProductProjectMembership::Inline(files) => {
                        push_count(output, files.len());
                        for file in files.iter() {
                            output.extend_from_slice(file);
                        }
                    }
                    ProductProjectMembership::Paged {
                        file_count,
                        page_keys,
                    } => {
                        output.push(1);
                        output.extend_from_slice(&file_count.to_be_bytes());
                        push_count(output, page_keys.len());
                        for page_key in page_keys.iter() {
                            output.extend_from_slice(page_key);
                        }
                        match cargo_aliases {
                            Some(evidence) => {
                                output.push(1);
                                encode_cargo_package_alias_evidence(evidence, output);
                            }
                            None => output.push(0),
                        }
                    }
                }
                if matches!(files, ProductProjectMembership::Inline(_))
                    && let Some(evidence) = cargo_aliases
                {
                    encode_cargo_package_alias_evidence(evidence, output);
                }
            }
            ProductSourceRecord::File { .. } => encode_file_record(value, output),
            ProductSourceRecord::MembershipPage { project, files } => {
                output.push(3);
                output.extend_from_slice(project);
                push_count(output, files.len());
                for file in files.iter() {
                    output.extend_from_slice(file);
                }
            }
        }
    }
}

/// Encodes one file record body after the format tag.
fn encode_file_record(value: &ProductSourceRecord, output: &mut Vec<u8>) {
    let ProductSourceRecord::File {
        project,
        path,
        language,
        content_version,
        analysis_version,
        source_identity,
        declarations,
        retention,
    } = value
    else {
        return;
    };
    let format = source_record_format(value);
    let factual = format == SOURCE_RECORD_FORMAT_FACTS;
    let identified = !factual && source_identity.is_some();
    debug_assert_eq!(
        identified,
        format == SOURCE_RECORD_FORMAT_IDENTIFIED,
        "the minimal format tag must name the identity exactly when one is stated"
    );
    let contained = format == SOURCE_RECORD_FORMAT_CONTAINED;
    output.push(2);
    output.extend_from_slice(project);
    push_text(output, path);
    output.push(language.wire_tag());
    output.extend_from_slice(content_version);
    output.extend_from_slice(analysis_version);
    if factual {
        match source_identity {
            Some(identity) => {
                output.push(1);
                output.extend_from_slice(identity.as_ref());
            }
            None => output.push(0),
        }
    } else if let Some(identity) = source_identity {
        output.extend_from_slice(identity.as_ref());
    }
    push_retention(output, *retention);
    push_count(output, declarations.len());
    for declaration in declarations.iter() {
        output.extend_from_slice(&declaration.line().to_be_bytes());
        // The file relation remains the authority for the selected path.
        // Retaining a declaration's typed location is useful for callers, but
        // an inconsistent path must not silently alter the file identity.
        push_text(output, path);
        push_text(output, declaration.name());
        output.push(declaration.kind().wire_tag());
        push_text(output, declaration.signature());
        push_text(output, declaration.documentation());
        push_excerpt(output, declaration.source_excerpt());
        if identified || factual {
            // The identified and facts formats carry containment
            // unconditionally: each tag already spends the format
            // discriminant, so containment cannot also select the tag.
            push_container(output, declaration.container());
        } else if contained {
            push_container(output, declaration.container());
        }
        if factual {
            push_facts(output, declaration.facts());
        }
    }
}

/// Encodes one declaration's observed facts. Each fact is `0` unobserved,
/// `1` absent, or `2` followed by its value.
fn push_facts(output: &mut Vec<u8>, facts: &DeclarationFacts) {
    match &facts.deprecation {
        Fact::Unobserved => output.push(0),
        Fact::Absent => output.push(1),
        Fact::Present(notice) => {
            output.push(2);
            push_optional_text(output, notice.since());
            push_optional_text(output, notice.note());
        }
    }
    match &facts.obligation {
        Fact::Unobserved => output.push(0),
        Fact::Absent => output.push(1),
        Fact::Present(obligation) => {
            output.push(2);
            output.push(obligation.wire_tag());
        }
    }
}

fn push_optional_text(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(text) => {
            output.push(1);
            push_text(output, text);
        }
        None => output.push(0),
    }
}

/// Encodes one declaration's extracted containment.
fn push_container(output: &mut Vec<u8>, container: &Container) {
    match container {
        Container::Module => output.push(0),
        Container::Enclosing { name, line } => {
            output.push(1);
            push_text(output, name);
            output.extend_from_slice(&line.get().to_be_bytes());
        }
        Container::Attached { type_name } => {
            output.push(2);
            push_text(output, type_name);
        }
    }
}

/// Encodes one declaration excerpt, including its typed absence markers.
fn push_excerpt(output: &mut Vec<u8>, excerpt: &SourceExcerpt) {
    match excerpt {
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

/// Number of leading relation-key bytes reserved for the owning project.
const PRODUCT_SOURCE_FILE_PROJECT_PREFIX_BYTES: usize = 16;

/// Number of relation-key bytes retained from the file identity digest.
///
/// The digest remains 128 bits, including when a project has many files. This
/// gives B-tree order a project-affine prefix while keeping the established
/// collision margin within one project's file set.
const PRODUCT_SOURCE_FILE_DIGEST_BYTES: usize = 16;

/// The byte layout for an independently keyed source file.
///
/// This type is intentionally internal: the relation and its callers already
/// use fixed-width byte keys, while construction here keeps the locality
/// prefix and collision-resistant suffix explicit in one place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProductSourceFileKey {
    project_prefix: [u8; PRODUCT_SOURCE_FILE_PROJECT_PREFIX_BYTES],
    file_digest: [u8; PRODUCT_SOURCE_FILE_DIGEST_BYTES],
}

impl ProductSourceFileKey {
    fn new(project: [u8; 32], path: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        // Version the new layout independently from the former full-digest
        // key. The complete project identity is included in the digest as
        // well as its prefix, so equal prefixes from distinct projects do not
        // erase project identity from the file suffix.
        hasher.update(b"backend.product-source.file-key.v2\0");
        hasher.update(&project);
        hasher.update(&(path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        let digest = hasher.finalize();

        let mut project_prefix = [0; PRODUCT_SOURCE_FILE_PROJECT_PREFIX_BYTES];
        project_prefix.copy_from_slice(&project[..PRODUCT_SOURCE_FILE_PROJECT_PREFIX_BYTES]);
        let mut file_digest = [0; PRODUCT_SOURCE_FILE_DIGEST_BYTES];
        file_digest.copy_from_slice(&digest.as_bytes()[..PRODUCT_SOURCE_FILE_DIGEST_BYTES]);
        Self {
            project_prefix,
            file_digest,
        }
    }

    fn to_bytes(self) -> [u8; 32] {
        let mut bytes = [0; 32];
        bytes[..PRODUCT_SOURCE_FILE_PROJECT_PREFIX_BYTES].copy_from_slice(&self.project_prefix);
        bytes[PRODUCT_SOURCE_FILE_PROJECT_PREFIX_BYTES..].copy_from_slice(&self.file_digest);
        bytes
    }
}

/// Derives the relation key for a canonical project-relative source file.
///
/// The first 16 bytes are the owning project's key prefix. The remaining 16
/// bytes are a domain-separated BLAKE3 digest over the full project key and
/// exact canonical path bytes. Fixed-width encoding preserves the relation's
/// byte ordering and lets all files for one project occupy a contiguous key
/// range.
#[must_use]
pub fn product_source_file_key(project: [u8; 32], path: &str) -> [u8; 32] {
    ProductSourceFileKey::new(project, path).to_bytes()
}

/// The key a build before the project-affine layout gave a source file: one
/// 32-byte BLAKE3 digest over `backend.product-source.file.v1`, the full
/// project key and the exact path bytes.
///
/// Nothing writes this key any more. It is kept so a workspace written by such
/// a build is recognised as state from another build, not reported as a
/// corrupt project frontier: its records decode (`SOURCE_RECORD_VERSION` did
/// not change) and only their keys differ.
#[must_use]
pub fn legacy_product_source_file_key(project: [u8; 32], path: &str) -> [u8; 32] {
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

fn push_retention(output: &mut Vec<u8>, retention: DeclarationRetention) {
    match retention {
        DeclarationRetention::Complete => output.push(0),
        DeclarationRetention::ExcerptsElided => output.push(1),
        DeclarationRetention::NamesOnly => output.push(2),
        DeclarationRetention::Truncated(counts) => {
            output.push(3);
            output.extend_from_slice(&counts.retained().to_be_bytes());
            output.extend_from_slice(&counts.extracted().to_be_bytes());
        }
        DeclarationRetention::Unavailable(reason) => {
            output.push(4);
            output.push(match reason {
                SourceUnavailableReason::Unreadable => 0,
                SourceUnavailableReason::NotText => 1,
                SourceUnavailableReason::TooLarge => 2,
                SourceUnavailableReason::Unparsed => 3,
            });
        }
    }
}

fn push_text(output: &mut Vec<u8>, value: &str) {
    push_count(output, value.len());
    output.extend_from_slice(value.as_bytes());
}

fn encode_cargo_package_alias_evidence(
    evidence: &CargoPackageAliasEvidenceV1,
    output: &mut Vec<u8>,
) {
    push_count(output, evidence.aliases().len());
    for alias in evidence.aliases() {
        output.push(alias.kind_tag());
        push_text(output, alias.as_str());
    }
    push_count(output, evidence.observations().len());
    for observation in evidence.observations() {
        output.extend_from_slice(&<[u8; 2]>::from(observation.profile()));
        output.extend_from_slice(&observation.source_observation_revision());
        match observation.cargo_facts() {
            Some(facts) => {
                output.push(1);
                output.extend_from_slice(&facts.metadata_response_digest());
                output.extend_from_slice(&facts.resolved_lockfile_digest());
                output.extend_from_slice(&facts.toolchain_binding_digest());
            }
            None => output.push(0),
        }
        match observation.coverage() {
            CargoPackageAliasCoverageV1::Complete => output.push(0),
            CargoPackageAliasCoverageV1::Truncated { retained, omitted } => {
                output.push(1);
                output.push(retained);
                output.extend_from_slice(&omitted.to_be_bytes());
            }
            CargoPackageAliasCoverageV1::Unavailable(reason) => {
                output.push(2);
                output.push(cargo_alias_unavailable_tag(reason));
            }
        }
        push_count(output, observation.alias_indices().len());
        output.extend_from_slice(observation.alias_indices());
    }
}

fn decode_cargo_package_alias_evidence(
    reader: &mut SourceReader<'_>,
) -> Result<CargoPackageAliasEvidenceV1, ()> {
    let alias_count = reader.count(MAX_CARGO_PACKAGE_ALIASES)?;
    let mut aliases = Vec::with_capacity(alias_count);
    for _ in 0..alias_count {
        let kind = reader.byte()?;
        let value = reader.text(MAX_RUST_CARGO_METADATA_TEXT_BYTES)?;
        aliases.push(
            CargoPackageAliasV1::from_wire_parts(kind, value.into_boxed_str()).map_err(|_| ())?,
        );
    }
    let observation_count = reader.count(MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS)?;
    let mut observations = Vec::with_capacity(observation_count);
    for _ in 0..observation_count {
        let profile_bytes: [u8; 2] = reader.take(2)?.try_into().map_err(|_| ())?;
        let profile = LanguageProfile::try_from(profile_bytes).map_err(|_| ())?;
        let source_observation_revision = reader.array()?;
        let cargo_facts = match reader.byte()? {
            0 => None,
            1 => Some(CargoPackageAliasCargoFactsV1::from_wire_parts(
                reader.array()?,
                reader.array()?,
                reader.array()?,
            )),
            _ => return Err(()),
        };
        let coverage = match reader.byte()? {
            0 => CargoPackageAliasCoverageV1::Complete,
            1 => CargoPackageAliasCoverageV1::Truncated {
                retained: reader.byte()?,
                omitted: reader.u32()?,
            },
            2 => CargoPackageAliasCoverageV1::Unavailable(
                cargo_alias_unavailable_from_tag(reader.byte()?).ok_or(())?,
            ),
            _ => return Err(()),
        };
        let index_count = reader.count(MAX_CARGO_PACKAGE_ALIASES)?;
        let mut alias_indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            alias_indices.push(reader.byte()?);
        }
        observations.push(
            CargoPackageAliasObservationV1::from_wire_parts(
                profile,
                source_observation_revision,
                cargo_facts,
                alias_indices,
                coverage,
            )
            .map_err(|_| ())?,
        );
    }
    CargoPackageAliasEvidenceV1::from_wire_parts(aliases, observations).map_err(|_| ())
}

const fn cargo_alias_unavailable_tag(reason: CargoPackageAliasUnavailableV1) -> u8 {
    match reason {
        CargoPackageAliasUnavailableV1::MetadataFactsUnavailable => 0,
        CargoPackageAliasUnavailableV1::NoExactWorkspaceMember => 1,
        CargoPackageAliasUnavailableV1::SelectedManifestMismatch => 2,
        CargoPackageAliasUnavailableV1::InvalidWorkspaceFacts => 3,
        CargoPackageAliasUnavailableV1::ProjectRowCapacity => 4,
    }
}

const fn cargo_alias_unavailable_from_tag(tag: u8) -> Option<CargoPackageAliasUnavailableV1> {
    match tag {
        0 => Some(CargoPackageAliasUnavailableV1::MetadataFactsUnavailable),
        1 => Some(CargoPackageAliasUnavailableV1::NoExactWorkspaceMember),
        2 => Some(CargoPackageAliasUnavailableV1::SelectedManifestMismatch),
        3 => Some(CargoPackageAliasUnavailableV1::InvalidWorkspaceFacts),
        4 => Some(CargoPackageAliasUnavailableV1::ProjectRowCapacity),
        _ => None,
    }
}

fn decode_source_record(bytes: &[u8]) -> Result<ProductSourceRecord, ()> {
    let mut reader = SourceReader::new(bytes);
    let version = format_version(reader.take(4)?).ok_or(())?;
    let record = match reader.byte()? {
        1 => {
            let label = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let source_version = reader.array()?;
            if version >= 13 {
                if reader.byte()? != 1 {
                    return Err(());
                }
                let file_count = reader.u32()? as usize;
                let page_count = reader.count(ProductSourceRecord::MAX_PROJECT_MEMBERSHIP_PAGES)?;
                let mut page_keys = Vec::with_capacity(page_count);
                for _ in 0..page_count {
                    page_keys.push(reader.array()?);
                }
                let aliases = match reader.byte()? {
                    0 => None,
                    1 => Some(decode_cargo_package_alias_evidence(&mut reader)?),
                    _ => return Err(()),
                };
                ProductSourceRecord::paged_project_with_aliases(
                    label,
                    source_version,
                    file_count,
                    page_keys,
                    aliases,
                )
                .map_err(|_| ())?
            } else {
                let count = reader.count(ProductSourceRecord::MAX_PROJECT_FILES)?;
                let mut files = Vec::with_capacity(count);
                for _ in 0..count {
                    files.push(reader.array()?);
                }
                if version >= 12 {
                    let aliases = decode_cargo_package_alias_evidence(&mut reader)?;
                    ProductSourceRecord::project_with_cargo_aliases(
                        label,
                        source_version,
                        files,
                        aliases,
                    )
                    .map_err(|_| ())?
                } else {
                    ProductSourceRecord::project(label, source_version, files).map_err(|_| ())?
                }
            }
        }
        2 if version < 12 => decode_file_record(&mut reader, version)?,
        3 if version >= 13 => {
            let project = reader.array()?;
            let count = reader.count(MAX_PROJECT_MEMBERSHIP_PAGE_FILES)?;
            let mut files = Vec::with_capacity(count);
            for _ in 0..count {
                files.push(reader.array()?);
            }
            ProductSourceRecord::membership_page(project, files).map_err(|_| ())?
        }
        _ => return Err(()),
    };
    reader.finish()?;
    Ok(record)
}

/// Decodes one file record body for any admitted `PSR*` format.
fn decode_file_record(
    reader: &mut SourceReader<'_>,
    version: u8,
) -> Result<ProductSourceRecord, ()> {
    let project = reader.array()?;
    let path = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
    let language = SourceLanguage::from_wire_tag(reader.byte()?).ok_or(())?;
    let content_version = reader.array()?;
    let analysis_version = if version >= 3 {
        reader.array()?
    } else {
        [0; 32]
    };
    let source_identity = if version >= 11 {
        match reader.byte()? {
            0 => None,
            1 => Some(ContentId::<SourceFactDomain>::try_from(reader.array()?).map_err(|_| ())?),
            _ => return Err(()),
        }
    } else if version >= 10 {
        Some(ContentId::<SourceFactDomain>::try_from(reader.array()?).map_err(|_| ())?)
    } else {
        None
    };
    let retention = if version >= 8 {
        decode_retention(reader)?
    } else {
        DeclarationRetention::Complete
    };
    let count = reader.count(ProductSourceRecord::MAX_FILE_DECLARATIONS)?;
    // Containment is tag-selected: `PSR9`, `PSRA`, and `PSRB` carry it, one
    // per declaration, while every earlier format implies the file module.
    let contained = version >= 9;
    let mut declarations = Vec::with_capacity(count);
    for _ in 0..count {
        declarations.push(decode_declaration(reader, version, &path, contained)?);
    }
    if version == 6 {
        skip_legacy_semantics(reader)?;
    }
    let record = ProductSourceRecord::file_with_retention(
        project,
        path,
        language,
        content_version,
        analysis_version,
        declarations,
        retention,
    )
    .map_err(|_| ())?;
    match source_identity {
        Some(identity) if matches!(record, ProductSourceRecord::File { .. }) => {
            record.with_source_identity(identity).map_err(|_| ())
        }
        _ => Ok(record),
    }
}

/// Decodes one declaration, tolerating the older pre-`PSR4` field shapes.
fn decode_declaration(
    reader: &mut SourceReader<'_>,
    version: u8,
    path: &str,
    contained: bool,
) -> Result<SourceDeclaration, ()> {
    let line = reader.u32()?;
    let declaration_path = if version >= 4 {
        reader.text(SourceLocation::MAX_PATH_BYTES)?
    } else {
        path.to_owned()
    };
    if declaration_path != path {
        return Err(());
    }
    let name = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
    let kind = if version >= 4 {
        DeclarationKind::from_wire_tag(reader.byte()?).ok_or(())?
    } else {
        DeclarationKind::from_name(&reader.text(SourceDeclaration::MAX_TEXT_BYTES)?)
    };
    let signature = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
    let documentation = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
    let source_excerpt = if version >= 5 {
        decode_source_excerpt(reader)?
    } else {
        SourceExcerpt::NotCaptured
    };
    let decoded_container = if contained {
        decode_container(reader)?
    } else {
        Container::Module
    };
    let facts = if version >= 11 {
        decode_facts(reader)?
    } else {
        DeclarationFacts::UNOBSERVED
    };
    Ok(SourceDeclaration::with_location(
        SourceLocation::new(declaration_path, line).map_err(|_| ())?,
        name,
        kind.name(),
        signature,
        documentation,
    )
    .map_err(|_| ())?
    .with_source_excerpt(source_excerpt)
    .with_container(decoded_container)
    .with_facts(facts))
}

/// Decodes one declaration's observed facts, rejecting an unknown tag.
fn decode_facts(reader: &mut SourceReader<'_>) -> Result<DeclarationFacts, ()> {
    let deprecation = match reader.byte()? {
        0 => Fact::Unobserved,
        1 => Fact::Absent,
        2 => {
            let since = decode_optional_text(reader)?;
            let note = decode_optional_text(reader)?;
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

fn decode_optional_text(reader: &mut SourceReader<'_>) -> Result<Option<String>, ()> {
    match reader.byte()? {
        0 => Ok(None),
        1 => reader.text(MAX_FACT_TEXT_BYTES).map(Some),
        _ => Err(()),
    }
}
/// Decodes one declaration's containment, rejecting an unknown discriminant.
fn decode_container(reader: &mut SourceReader<'_>) -> Result<Container, ()> {
    match reader.byte()? {
        0 => Ok(Container::Module),
        1 => {
            let name = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
            let line = NonZeroU32::new(reader.u32()?).ok_or(())?;
            Ok(Container::enclosing(&name, line))
        }
        2 => Ok(Container::attached(
            &reader.text(SourceDeclaration::MAX_TEXT_BYTES)?,
        )),
        _ => Err(()),
    }
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

fn decode_retention(reader: &mut SourceReader<'_>) -> Result<DeclarationRetention, ()> {
    match reader.byte()? {
        0 => Ok(DeclarationRetention::Complete),
        1 => Ok(DeclarationRetention::ExcerptsElided),
        2 => Ok(DeclarationRetention::NamesOnly),
        3 => {
            let retained = reader.u32()?;
            let extracted = reader.u32()?;
            RetainedDeclarations::new(retained, extracted)
                .map(DeclarationRetention::Truncated)
                .map_err(|_| ())
        }
        4 => match reader.byte()? {
            0 => Ok(DeclarationRetention::Unavailable(
                SourceUnavailableReason::Unreadable,
            )),
            1 => Ok(DeclarationRetention::Unavailable(
                SourceUnavailableReason::NotText,
            )),
            2 => Ok(DeclarationRetention::Unavailable(
                SourceUnavailableReason::TooLarge,
            )),
            3 => Ok(DeclarationRetention::Unavailable(
                SourceUnavailableReason::Unparsed,
            )),
            _ => Err(()),
        },
        _ => Err(()),
    }
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
            files: ProductProjectMembership::Inline(Arc::from([])),
            cargo_aliases: None,
        },
    )];
    if expanded {
        entries.push((
            backend_library::package_key("backend-extra").to_bytes(),
            ProductSourceRecord::Project {
                label: "backend-extra".to_owned(),
                source_version: [0; 32],
                files: ProductProjectMembership::Inline(Arc::from([])),
                cargo_aliases: None,
            },
        ));
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::{
        DeclarationKind, DeclarationRetention, MAX_PROJECT_MEMBERSHIP_PAGE_FILES,
        MIN_PROJECT_MEMBERSHIP_PAGE_FILES, ProductProjectFileMembership, ProductProjectMembership,
        ProductSourceRecord, ProductSourceRelation, Relation, RetainedDeclarations,
        SourceDeclaration, SourceExcerpt, SourceExcerptExtent, SourceLocation,
        SourceUnavailableReason,
    };
    use backend_version::{CanonicalRelation, ContentId, SourceFactDomain};
    use std::sync::Arc;

    fn cargo_alias_evidence() -> backend_library::CargoPackageAliasEvidenceV1 {
        use backend_library::{
            CargoPackageAliasCargoFactsV1, CargoPackageAliasCoverageV1,
            CargoPackageAliasEvidenceV1, CargoPackageAliasObservationV1, CargoPackageAliasV1,
            CargoPackageNameV1, CargoTargetNameV1,
        };
        use backend_semantic::vocabulary::{LanguageProfile, RustEdition};

        let aliases = vec![
            CargoPackageAliasV1::CargoPackageName(
                CargoPackageNameV1::new("display-package").expect("package alias"),
            ),
            CargoPackageAliasV1::CargoTargetName(
                CargoTargetNameV1::new("rust_crate").expect("target alias"),
            ),
        ];
        let observation = CargoPackageAliasObservationV1::from_wire_parts(
            LanguageProfile::Rust(RustEdition::Rust2021),
            [21; 32],
            Some(CargoPackageAliasCargoFactsV1::from_wire_parts(
                [22; 32], [23; 32], [24; 32],
            )),
            vec![0, 1],
            CargoPackageAliasCoverageV1::Complete,
        )
        .expect("profile observation");
        CargoPackageAliasEvidenceV1::from_wire_parts(aliases, vec![observation])
            .expect("alias evidence")
    }

    fn ordered_membership_keys(count: usize) -> Vec<[u8; 32]> {
        (0..count)
            .map(|index| {
                let mut key = [0x5a; 32];
                let index = u64::try_from(index).expect("fixture frontier index fits u64");
                key[..8].copy_from_slice(&index.to_be_bytes());
                key
            })
            .collect()
    }

    fn page_rows(
        update: &super::ProductSourceProjectUpdate,
    ) -> std::collections::BTreeMap<[u8; 32], ProductSourceRecord> {
        update.membership_pages().iter().cloned().collect()
    }

    #[test]
    fn membership_builder_keeps_small_frontiers_inline_and_pages_oversized_rows() {
        for count in [
            0,
            1,
            ProductSourceRecord::MAX_FRONTIER_FILES,
            ProductSourceRecord::MAX_FRONTIER_FILES + 1,
            2_043,
            10_000,
        ] {
            let files = ordered_membership_keys(count);
            let update = ProductSourceRecord::project_with_membership_pages(
                "fixture",
                [7; 32],
                files.clone(),
                None,
            )
            .expect("membership update");
            let project_key = backend_library::package_key("fixture").to_bytes();
            let project = update.project_record().project_fields().expect("project");
            let pages = page_rows(&update);
            let resolved = project
                .iter_file_keys(project_key, |key| Ok(pages.get(key).cloned()))
                .collect::<Result<Vec<_>, _>>()
                .expect("complete membership");
            assert_eq!(resolved, files, "file count {count}");
            assert!(
                update.project_record().encoded_value_bytes()
                    <= ProductSourceRecord::ROW_VALUE_CAPACITY
            );
            if count <= ProductSourceRecord::MAX_FRONTIER_FILES {
                assert!(matches!(
                    project.files,
                    ProductProjectFileMembership::Inline(_)
                ));
                assert!(update.membership_pages().is_empty());
            } else {
                let ProductProjectFileMembership::Paged {
                    file_count,
                    page_keys,
                } = project.files
                else {
                    panic!("oversized inline row must use pages");
                };
                assert_eq!(file_count, count);
                assert!(page_keys.len() <= ProductSourceRecord::MAX_PROJECT_MEMBERSHIP_PAGES);
                assert_eq!(page_keys.len(), update.membership_pages().len());
                for (index, (page_key, page)) in update.membership_pages().iter().enumerate() {
                    let fields = page.membership_page_fields().expect("page row");
                    assert_eq!(fields.project, project_key);
                    assert!(fields.files.len() <= MAX_PROJECT_MEMBERSHIP_PAGE_FILES);
                    if index + 1 < update.membership_pages().len() {
                        assert!(fields.files.len() >= MIN_PROJECT_MEMBERSHIP_PAGE_FILES);
                    }
                    assert_eq!(
                        super::product_source_membership_page_key(project_key, fields.files),
                        Ok(*page_key)
                    );
                    assert!(page.encoded_value_bytes() <= ProductSourceRecord::ROW_VALUE_CAPACITY);
                    let mut encoded = Vec::new();
                    ProductSourceRelation::encode_value(page, &mut encoded);
                    let decoded = ProductSourceRelation::decode_value(&encoded).expect("PSRD page");
                    assert_eq!(decoded, *page);
                }
            }
        }
    }

    #[test]
    fn inline_alias_overflow_falls_back_to_pages_before_degrading_alias_evidence() {
        let label = "x".repeat(ProductSourceRecord::MAX_LABEL_BYTES);
        let aliases = cargo_alias_evidence();
        let update = ProductSourceRecord::project_with_membership_pages(
            label,
            [7; 32],
            ordered_membership_keys(ProductSourceRecord::MAX_FRONTIER_FILES),
            Some(aliases.clone()),
        )
        .expect("paged row leaves room for exact aliases");
        let fields = update.project_record().project_fields().expect("project");
        assert!(matches!(
            fields.files,
            ProductProjectFileMembership::Paged { .. }
        ));
        assert_eq!(fields.cargo_aliases, Some(&aliases));
        assert!(
            update.project_record().encoded_value_bytes()
                <= ProductSourceRecord::ROW_VALUE_CAPACITY
        );
    }

    #[test]
    fn maximum_project_membership_has_bounded_page_rows_and_exact_count() {
        let files = ordered_membership_keys(ProductSourceRecord::MAX_PROJECT_FILES);
        let update = ProductSourceRecord::project_with_membership_pages(
            "fixture",
            [8; 32],
            files.clone(),
            None,
        )
        .expect("maximum logical frontier");
        let project_key = backend_library::package_key("fixture").to_bytes();
        let fields = update.project_record().project_fields().expect("project");
        let ProductProjectFileMembership::Paged {
            file_count,
            page_keys,
        } = fields.files
        else {
            panic!("maximum frontier must use pages");
        };
        assert_eq!(file_count, ProductSourceRecord::MAX_PROJECT_FILES);
        assert!(page_keys.len() <= ProductSourceRecord::MAX_PROJECT_MEMBERSHIP_PAGES);
        let mut row_bytes = Vec::new();
        ProductSourceRelation::encode_value(update.project_record(), &mut row_bytes);
        assert!(row_bytes.len() <= ProductSourceRecord::ROW_VALUE_CAPACITY);
        let pages = page_rows(&update);
        let resolved = fields
            .iter_file_keys(project_key, |key| Ok(pages.get(key).cloned()))
            .collect::<Result<Vec<_>, _>>()
            .expect("resolve all membership pages");
        assert_eq!(resolved, files);
        assert_eq!(resolved.len(), ProductSourceRecord::MAX_PROJECT_FILES);
        assert_eq!(pages.len(), page_keys.len());
    }

    #[test]
    fn membership_page_identity_and_ownership_fail_closed() {
        let files = ordered_membership_keys(2_043);
        let update =
            ProductSourceRecord::project_with_membership_pages("fixture", [9; 32], files, None)
                .expect("paged project");
        let project_key = update.project_key();
        let fields = update.project_record().project_fields().expect("project");
        let pages = page_rows(&update);
        let ProductProjectFileMembership::Paged { page_keys, .. } = fields.files else {
            panic!("fixture should be paged");
        };
        let missing = fields
            .iter_file_keys(project_key, |_| Ok(None))
            .collect::<Result<Vec<_>, _>>()
            .expect_err("a missing page invalidates the complete frontier");
        assert!(missing.contains("missing"));

        let first_key = page_keys[0];
        let first = pages.get(&first_key).expect("first page");
        let first_files = first.membership_page_fields().expect("page").files.to_vec();
        let foreign = ProductSourceRecord::membership_page([0x33; 32], first_files.clone())
            .expect("foreign page");
        let wrong_owner = fields
            .iter_file_keys(project_key, |key| {
                Ok(if *key == first_key {
                    Some(foreign.clone())
                } else {
                    pages.get(key).cloned()
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .expect_err("cross-project page must fail");
        assert!(wrong_owner.contains("another project"));

        let mut corrupt_files = first_files;
        corrupt_files[0][31] ^= 1;
        let corrupt = ProductSourceRecord::membership_page(project_key, corrupt_files)
            .expect("well-formed but wrongly keyed page");
        let bad_identity = fields
            .iter_file_keys(project_key, |key| {
                Ok(if *key == first_key {
                    Some(corrupt.clone())
                } else {
                    pages.get(key).cloned()
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .expect_err("page payload must match its content-derived key");
        assert!(bad_identity.contains("identity"));
    }

    #[test]
    fn membership_pages_reject_duplicate_references_overlaps_and_wrong_totals() {
        assert!(super::validate_paged_membership(512, &[[1; 32], [1; 32]]).is_err());

        let project_key = backend_library::package_key("overlap").to_bytes();
        let keys = ordered_membership_keys(513);
        let first_files = keys[..256].to_vec();
        let second_files = keys[255..].to_vec();
        let first = ProductSourceRecord::membership_page(project_key, first_files.clone())
            .expect("first bounded page");
        let second = ProductSourceRecord::membership_page(project_key, second_files.clone())
            .expect("second bounded page");
        let first_key = super::product_source_membership_page_key(project_key, &first_files)
            .expect("first page identity");
        let second_key = super::product_source_membership_page_key(project_key, &second_files)
            .expect("second page identity");
        let rows = std::collections::BTreeMap::from([(first_key, first), (second_key, second)]);
        let overlap = ProductSourceRecord::paged_project_with_aliases(
            "overlap".to_owned(),
            [1; 32],
            514,
            vec![first_key, second_key],
            None,
        )
        .expect("bounded but overlapping project root");
        let error = overlap
            .project_fields()
            .expect("project fields")
            .iter_file_keys(project_key, |key| Ok(rows.get(key).cloned()))
            .collect::<Result<Vec<_>, _>>()
            .expect_err("overlapping page ranges must fail closed");
        assert!(error.contains("unordered or duplicate"));

        let update = ProductSourceRecord::project_with_membership_pages(
            "fixture",
            [2; 32],
            ordered_membership_keys(2_043),
            None,
        )
        .expect("valid project update");
        let fields = update.project_record().project_fields().expect("project");
        let ProductProjectFileMembership::Paged { page_keys, .. } = fields.files else {
            panic!("fixture should be paged");
        };
        let wrong_total = ProductSourceRecord::Project {
            label: "fixture".to_owned(),
            source_version: [2; 32],
            files: ProductProjectMembership::Paged {
                file_count: 2_044,
                page_keys: Arc::from(page_keys.to_vec().into_boxed_slice()),
            },
            cargo_aliases: None,
        };
        let pages = page_rows(&update);
        let error = wrong_total
            .project_fields()
            .expect("project fields")
            .iter_file_keys(update.project_key(), |key| Ok(pages.get(key).cloned()))
            .collect::<Result<Vec<_>, _>>()
            .expect_err("declared membership count must match resolved pages");
        assert!(error.contains("count does not match"));
    }

    #[test]
    fn content_defined_membership_pages_reuse_unchanged_suffix_after_insert_delete() {
        let mut files = ordered_membership_keys(10_000);
        let project = backend_library::package_key("fixture").to_bytes();
        let original = ProductSourceRecord::project_with_membership_pages(
            "fixture",
            [10; 32],
            files.clone(),
            None,
        )
        .expect("original pages");
        let original_pages = original
            .membership_pages()
            .iter()
            .map(|(key, _)| *key)
            .collect::<std::collections::BTreeSet<_>>();

        let inserted = [0; 32];
        files.insert(0, inserted);
        let after_insert = ProductSourceRecord::project_with_membership_pages(
            "fixture",
            [11; 32],
            files.clone(),
            None,
        )
        .expect("pages after insertion");
        let inserted_pages = after_insert
            .membership_pages()
            .iter()
            .map(|(key, _)| *key)
            .collect::<std::collections::BTreeSet<_>>();
        let reused_after_insert = original_pages.intersection(&inserted_pages).count();
        assert!(
            reused_after_insert > 0,
            "content-defined seams should preserve some unchanged page identities"
        );

        files.remove(1);
        let after_delete =
            ProductSourceRecord::project_with_membership_pages("fixture", [12; 32], files, None)
                .expect("pages after deletion");
        let deleted_pages = after_delete
            .membership_pages()
            .iter()
            .map(|(key, _)| *key)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(original_pages.intersection(&deleted_pages).count() > 0);
        assert!(after_insert.project_key() == project);
    }

    #[test]
    fn source_file_keys_cluster_by_project_and_round_trip_canonically() {
        let project = [0x11; 32];
        let mut same_prefix_other_project = [0x11; 32];
        same_prefix_other_project[31] = 0x12;
        let later_project = [0x22; 32];
        let paths = ["src/z.rs", "src/a.rs", "src/lib.rs"];
        let mut keys = paths
            .iter()
            .map(|path| super::product_source_file_key(project, path))
            .collect::<Vec<_>>();

        assert!(keys.iter().all(|key| key[..16] == project[..16]));
        assert_ne!(keys[0], keys[1], "different paths keep distinct identities");
        assert_ne!(
            super::product_source_file_key(project, "src/lib.rs"),
            super::product_source_file_key(same_prefix_other_project, "src/lib.rs"),
            "the full project identity remains in the file digest"
        );

        let later_project_key = super::product_source_file_key(later_project, paths[0]);
        assert!(
            keys.iter().all(|key| *key < later_project_key),
            "the project prefix sorts this project's full key range together"
        );

        keys.sort_unstable();
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        let project_record = ProductSourceRecord::project("fixture", [0; 32], keys.clone())
            .expect("sorted file frontier");
        assert_eq!(
            project_record.project_fields().expect("project").files,
            super::ProductProjectFileMembership::Inline(keys.as_slice())
        );

        let mut key_bytes = Vec::new();
        ProductSourceRelation::encode_key(&keys[0], &mut key_bytes);
        let decoded = ProductSourceRelation::decode_key(&key_bytes).expect("decode key");
        let mut encoded_again = Vec::new();
        ProductSourceRelation::encode_key(&decoded, &mut encoded_again);
        assert_eq!(decoded, keys[0]);
        assert_eq!(encoded_again, key_bytes);
    }

    #[test]
    fn the_retired_source_file_key_layout_is_reproducible_and_never_the_current_one() {
        let project = [0x11; 32];
        // The retired layout, spelled out independently of the function under
        // test: one 32-byte digest, no project prefix.
        let mut retired = blake3::Hasher::new();
        retired.update(b"backend.product-source.file.v1\0");
        retired.update(&project);
        retired.update(&("src/lib.rs".len() as u64).to_be_bytes());
        retired.update(b"src/lib.rs");
        let retired = *retired.finalize().as_bytes();

        let legacy = super::legacy_product_source_file_key(project, "src/lib.rs");
        assert_eq!(legacy, retired);
        assert_ne!(
            legacy,
            super::product_source_file_key(project, "src/lib.rs"),
            "a workspace keyed this way is state from another build, not a current one"
        );
        assert_ne!(
            legacy[..16],
            project[..16],
            "the retired key carries no project prefix"
        );
    }

    #[test]
    fn a_file_key_collision_is_rejected_by_the_project_frontier() {
        let project = [0x31; 32];
        let key = super::product_source_file_key(project, "src/lib.rs");
        let error = ProductSourceRecord::project("fixture", [0; 32], vec![key, key])
            .expect_err("a colliding key cannot appear twice in a frontier");
        assert_eq!(
            error,
            "project file frontier is unordered or holds a duplicate"
        );
    }

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
        assert_eq!(
            encoded.get(..4),
            Some(super::SOURCE_RECORD_FORMAT_PLAIN.as_slice())
        );
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

    /// Writes the exact bytes the pre-containment encoder produced.
    ///
    /// The layout is spelled out rather than produced by the encoder under
    /// test, because a persisted intent embeds these bytes and restart
    /// admission compares them byte for byte: an encoder that silently starts
    /// writing one byte more stops every existing workspace from opening.
    fn legacy_file_record_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PSR8");
        bytes.push(2);
        bytes.extend_from_slice(&[3; 32]);
        super::push_text(&mut bytes, "src/lib.rs");
        bytes.push(super::SourceLanguage::Rust.wire_tag());
        bytes.extend_from_slice(&[4; 32]);
        bytes.extend_from_slice(&[5; 32]);
        bytes.push(0);
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&7_u32.to_be_bytes());
        super::push_text(&mut bytes, "src/lib.rs");
        super::push_text(&mut bytes, "answer");
        bytes.push(DeclarationKind::Function.wire_tag());
        super::push_text(&mut bytes, "fn answer() -> u32");
        super::push_text(&mut bytes, "the answer");
        bytes.push(0);
        bytes
    }

    fn legacy_file_record() -> ProductSourceRecord {
        let declaration = SourceDeclaration::at_path(
            "src/lib.rs",
            "answer",
            "function",
            7,
            "fn answer() -> u32",
            "the answer",
        )
        .expect("declaration");
        ProductSourceRecord::file(
            [3; 32],
            "src/lib.rs",
            super::SourceLanguage::Rust,
            [4; 32],
            [5; 32],
            Arc::from([declaration]),
        )
        .expect("file")
    }

    #[test]
    fn a_record_stating_no_containment_encodes_exactly_as_it_did_before() {
        let record = legacy_file_record();
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&record, &mut encoded);
        assert_eq!(encoded, legacy_file_record_bytes());
        let decoded =
            ProductSourceRelation::decode_value(&legacy_file_record_bytes()).expect("decode");
        assert_eq!(decoded, record);
        assert_eq!(
            decoded
                .file_fields()
                .and_then(|fields| fields.declarations.first())
                .map(SourceDeclaration::container),
            Some(&super::Container::Module)
        );
    }

    #[test]
    fn project_cargo_aliases_round_trip_without_changing_legacy_project_bytes() {
        let legacy =
            ProductSourceRecord::project("fixture", [7; 32], Vec::new()).expect("legacy project");
        let mut legacy_bytes = Vec::new();
        ProductSourceRelation::encode_value(&legacy, &mut legacy_bytes);
        assert_eq!(legacy_bytes.get(..4), Some(b"PSR8".as_slice()));
        let mut expected_legacy = b"PSR8\x01".to_vec();
        super::push_text(&mut expected_legacy, "fixture");
        expected_legacy.extend_from_slice(&[7; 32]);
        expected_legacy.extend_from_slice(&0_u32.to_be_bytes());
        assert_eq!(legacy_bytes, expected_legacy);

        let aliases = cargo_alias_evidence();
        let record = ProductSourceRecord::project_with_cargo_aliases(
            "fixture",
            [7; 32],
            Vec::new(),
            aliases.clone(),
        )
        .expect("project aliases");
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&record, &mut encoded);
        assert_eq!(encoded.get(..4), Some(b"PSRC".as_slice()));
        let decoded = ProductSourceRelation::decode_value(&encoded).expect("decode aliases");
        assert_eq!(decoded, record);
        assert_eq!(
            decoded
                .project_fields()
                .and_then(|fields| fields.cargo_aliases),
            Some(&aliases)
        );
        let mut reencoded = Vec::new();
        ProductSourceRelation::encode_value(&decoded, &mut reencoded);
        assert_eq!(reencoded, encoded);
    }

    #[test]
    fn source_record_format_version_accepts_only_canonical_tags() {
        for version in 2..=9_u8 {
            let tag = b'0' + version;
            assert_eq!(
                super::format_version(&[b'P', b'S', b'R', tag]),
                Some(version),
                "PSR{} remains a supported decimal version",
                version
            );
        }
        for (tag, version) in [(b'A', 10_u8), (b'B', 11_u8), (b'C', 12_u8), (b'D', 13_u8)] {
            assert_eq!(
                super::format_version(&[b'P', b'S', b'R', tag]),
                Some(version),
                "the named alphabetic version remains supported"
            );
        }

        for tag in [b'0', b'1', b':', b';', b'<', b'E', b'?', 0xff] {
            assert_eq!(
                super::format_version(&[b'P', b'S', b'R', tag]),
                None,
                "tag byte {tag:?} is not a canonical supported format"
            );
        }
    }

    #[test]
    fn source_record_decoder_rejects_punctuation_alias_tag_and_keeps_psr8() {
        let legacy =
            ProductSourceRecord::project("fixture", [7; 32], Vec::new()).expect("legacy project");
        let mut legacy_bytes = Vec::new();
        ProductSourceRelation::encode_value(&legacy, &mut legacy_bytes);
        assert_eq!(legacy_bytes.get(..4), Some(b"PSR8".as_slice()));
        let decoded_legacy =
            ProductSourceRelation::decode_value(&legacy_bytes).expect("decode legacy PSR8");
        assert_eq!(decoded_legacy, legacy);

        let aliased = ProductSourceRecord::project_with_cargo_aliases(
            "fixture",
            [7; 32],
            Vec::new(),
            cargo_alias_evidence(),
        )
        .expect("valid Cargo alias project");
        let mut valid_bytes = Vec::new();
        ProductSourceRelation::encode_value(&aliased, &mut valid_bytes);
        assert_eq!(valid_bytes.get(..4), Some(b"PSRC".as_slice()));
        let decoded_aliases =
            ProductSourceRelation::decode_value(&valid_bytes).expect("decode valid PSRC");
        assert_eq!(decoded_aliases, aliased);

        let mut punctuation_tagged = valid_bytes;
        punctuation_tagged[..4].copy_from_slice(b"PSR<");
        assert!(ProductSourceRelation::decode_value(&punctuation_tagged).is_err());
    }

    #[test]
    fn project_frontier_capacity_retains_explicit_alias_unavailability() {
        use backend_library::{
            CargoPackageAliasCargoFactsV1, CargoPackageAliasCoverageV1,
            CargoPackageAliasEvidenceV1, CargoPackageAliasObservationV1, CargoPackageAliasV1,
            CargoTargetNameV1, MAX_CARGO_PACKAGE_ALIASES,
        };
        use backend_semantic::vocabulary::{LanguageProfile, RustEdition};

        let aliases = (0..MAX_CARGO_PACKAGE_ALIASES)
            .map(|index| {
                CargoPackageAliasV1::CargoTargetName(
                    CargoTargetNameV1::new(format!("{index:02}-{}", "x".repeat(1_000)))
                        .expect("bounded target alias"),
                )
            })
            .collect::<Vec<_>>();
        let observation = CargoPackageAliasObservationV1::from_wire_parts(
            LanguageProfile::Rust(RustEdition::Rust2024),
            [31; 32],
            Some(CargoPackageAliasCargoFactsV1::from_wire_parts(
                [32; 32], [33; 32], [34; 32],
            )),
            (0..MAX_CARGO_PACKAGE_ALIASES)
                .map(|index| u8::try_from(index).expect("alias index"))
                .collect(),
            CargoPackageAliasCoverageV1::Complete,
        )
        .expect("profile observation");
        let evidence = CargoPackageAliasEvidenceV1::from_wire_parts(aliases, vec![observation])
            .expect("bounded large alias evidence");
        let files = (0..ProductSourceRecord::MAX_FRONTIER_FILES.saturating_sub(16))
            .map(|index| {
                let mut key = [0; 32];
                key[..8].copy_from_slice(&(index as u64).to_be_bytes());
                key
            })
            .collect::<Vec<_>>();
        let base = ProductSourceRecord::project("fixture", [7; 32], files.clone())
            .expect("base frontier fits without aliases");
        assert!(base.encoded_value_bytes() <= ProductSourceRecord::ROW_VALUE_CAPACITY);

        let project =
            ProductSourceRecord::project_with_cargo_aliases("fixture", [7; 32], files, evidence)
                .expect("over-capacity alias names become explicit unavailable coverage");
        let fields = project.project_fields().expect("project row");
        let aliases = fields.cargo_aliases.expect("typed alias coverage");
        assert!(aliases.aliases().is_empty());
        assert_eq!(
            aliases.observations()[0].coverage(),
            CargoPackageAliasCoverageV1::Unavailable(
                backend_library::CargoPackageAliasUnavailableV1::ProjectRowCapacity
            )
        );
        assert!(project.encoded_value_bytes() <= ProductSourceRecord::ROW_VALUE_CAPACITY);
    }

    /// The semantic source content identity of a record whose bytes the
    /// semantic compiler would hash the same way.
    fn fixture_source_identity() -> ContentId<SourceFactDomain> {
        ContentId::<SourceFactDomain>::from_canonical_bytes(b"fixture source bytes")
    }

    #[test]
    fn a_stated_source_identity_round_trips_and_earns_its_tag() {
        let record = legacy_file_record()
            .with_source_identity(fixture_source_identity())
            .expect("file record accepts an identity");
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&record, &mut encoded);
        assert_eq!(
            encoded.get(..4),
            Some(super::SOURCE_RECORD_FORMAT_IDENTIFIED.as_slice()),
            "stating an identity is exactly what the PSRA tag means"
        );
        let decoded = ProductSourceRelation::decode_value(&encoded).expect("decode");
        let fields = decoded.file_fields().expect("file fields");
        assert_eq!(fields.source_identity, Some(fixture_source_identity()));
        // Re-encoding is byte-identical, as restart admission requires.
        let mut again = Vec::new();
        ProductSourceRelation::encode_value(&decoded, &mut again);
        assert_eq!(again, encoded);
        // A project record has no bytes to name.
        let files = ProductSourceRecord::project("project", [1; 32], Vec::new()).expect("project");
        assert!(
            files
                .with_source_identity(fixture_source_identity())
                .is_err()
        );
        // A record without an identity still encodes exactly as before.
        let untagged = legacy_file_record();
        let mut plain = Vec::new();
        ProductSourceRelation::encode_value(&untagged, &mut plain);
        assert_eq!(plain, legacy_file_record_bytes());
    }

    #[test]
    fn containment_round_trips_and_a_redundant_contained_record_is_refused() {
        let declaration =
            SourceDeclaration::at_path("src/lib.rs", "run", "method", 11, "fn run(&self)", "")
                .expect("declaration")
                .with_container(super::Container::attached("Worker"));
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
        assert_eq!(encoded.get(..4), Some(b"PSR9".as_slice()));
        let decoded = ProductSourceRelation::decode_value(&encoded).expect("decode");
        assert_eq!(decoded, record);
        let mut round_tripped = Vec::new();
        ProductSourceRelation::encode_value(&decoded, &mut round_tripped);
        assert_eq!(round_tripped, encoded);

        // A record written by an encoder that always tagged `PSR9` is still
        // readable; it simply normalizes to the minimal tag when re-encoded.
        let mut redundant = legacy_file_record_bytes();
        redundant.splice(..4, b"PSR9".iter().copied());
        redundant.push(0);
        let normalized = ProductSourceRelation::decode_value(&redundant).expect("decode");
        assert_eq!(normalized, legacy_file_record());
        let mut re_encoded = Vec::new();
        ProductSourceRelation::encode_value(&normalized, &mut re_encoded);
        assert_eq!(re_encoded, legacy_file_record_bytes());
    }

    #[test]
    fn declaration_facts_round_trip_in_the_facts_format_only() {
        let deprecated = SourceDeclaration::at_path(
            "src/lib.rs",
            "stale",
            "function",
            7,
            "pub fn stale()",
            "Makes one.",
        )
        .expect("declaration")
        .with_facts(backend_compile::DeclarationFacts {
            deprecation: backend_compile::Fact::Present(backend_compile::Deprecation::new(
                Some("1.2.0"),
                Some("use `fresh`"),
            )),
            obligation: backend_compile::Fact::Absent,
        });
        let required = SourceDeclaration::at_path(
            "src/lib.rs",
            "execute",
            "method",
            11,
            "fn execute(&self)",
            "",
        )
        .expect("declaration")
        .with_container(super::Container::attached("Service"))
        .with_facts(backend_compile::DeclarationFacts {
            deprecation: backend_compile::Fact::Absent,
            obligation: backend_compile::Fact::Present(backend_compile::Obligation::Required),
        });
        let record = ProductSourceRecord::file(
            [3; 32],
            "src/lib.rs",
            super::SourceLanguage::Rust,
            [4; 32],
            [5; 32],
            Arc::from([deprecated, required]),
        )
        .expect("file");
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&record, &mut encoded);
        assert_eq!(encoded.get(..4), Some(b"PSRB".as_slice()));
        let decoded = ProductSourceRelation::decode_value(&encoded).expect("decode");
        let fields = decoded.file_fields().expect("file fields");
        let stale = fields
            .declarations
            .iter()
            .find(|declaration| declaration.name() == "stale")
            .expect("stale");
        let notice = stale.facts().deprecation.present().expect("deprecation");
        assert_eq!(notice.since(), Some("1.2.0"));
        assert_eq!(notice.note(), Some("use `fresh`"));
        let execute = fields
            .declarations
            .iter()
            .find(|declaration| declaration.name() == "execute")
            .expect("execute");
        assert_eq!(
            execute.facts().obligation,
            backend_compile::Fact::Present(backend_compile::Obligation::Required)
        );
        assert_eq!(execute.container(), &super::Container::attached("Service"));
        // Restart admission re-encodes byte-identically.
        let mut again = Vec::new();
        ProductSourceRelation::encode_value(&decoded, &mut again);
        assert_eq!(again, encoded);
        // The facts format states an identity with a presence byte.
        let identified = record
            .with_source_identity(fixture_source_identity())
            .expect("identity");
        let mut with_identity = Vec::new();
        ProductSourceRelation::encode_value(&identified, &mut with_identity);
        assert_eq!(with_identity.get(..4), Some(b"PSRB".as_slice()));
        let reopened = ProductSourceRelation::decode_value(&with_identity).expect("decode");
        assert_eq!(
            reopened.file_fields().expect("file").source_identity,
            Some(fixture_source_identity())
        );
        let mut reopened_bytes = Vec::new();
        ProductSourceRelation::encode_value(&reopened, &mut reopened_bytes);
        assert_eq!(reopened_bytes, with_identity);
        // A record whose producer observed nothing keeps its historical tag.
        let mut plain = Vec::new();
        ProductSourceRelation::encode_value(&legacy_file_record(), &mut plain);
        assert_eq!(plain, legacy_file_record_bytes());
    }

    /// Writes one file record in an exact historical format.
    ///
    /// Each earlier tag differs in which fields are present, and a workspace
    /// indexed by any of them must keep opening, so every shape is decoded
    /// here rather than trusted to a `matches!` list nobody exercises.
    fn file_record_bytes_for_version(version: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[b'P', b'S', b'R', b'0'.saturating_add(version)]);
        bytes.push(2);
        bytes.extend_from_slice(&[3; 32]);
        super::push_text(&mut bytes, "src/lib.rs");
        bytes.push(super::SourceLanguage::Rust.wire_tag());
        bytes.extend_from_slice(&[4; 32]);
        if version >= 3 {
            bytes.extend_from_slice(&[5; 32]);
        }
        if version >= 8 {
            bytes.push(0);
        }
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&7_u32.to_be_bytes());
        if version >= 4 {
            super::push_text(&mut bytes, "src/lib.rs");
        }
        super::push_text(&mut bytes, "answer");
        if version >= 4 {
            bytes.push(DeclarationKind::Function.wire_tag());
        } else {
            super::push_text(&mut bytes, "function");
        }
        super::push_text(&mut bytes, "fn answer() -> u32");
        super::push_text(&mut bytes, "the answer");
        if version >= 5 {
            bytes.push(0);
        }
        if version == 6 {
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn every_earlier_record_format_decodes_with_no_containment() {
        for version in 2..=8_u8 {
            let bytes = file_record_bytes_for_version(version);
            let decoded = ProductSourceRelation::decode_value(&bytes)
                .unwrap_or_else(|error| panic!("decode PSR{version}: {error:?}"));
            let fields = decoded
                .file_fields()
                .unwrap_or_else(|| panic!("PSR{version} file fields"));
            let declaration = fields
                .declarations
                .first()
                .unwrap_or_else(|| panic!("PSR{version} declaration"));
            assert_eq!(declaration.name(), "answer");
            assert_eq!(declaration.kind(), DeclarationKind::Function);
            assert_eq!(declaration.line(), 7);
            assert_eq!(declaration.container(), &super::Container::Module);
            assert_eq!(
                fields.analysis_version,
                if version >= 3 { [5; 32] } else { [0; 32] }
            );
        }
    }

    fn wide_declaration(index: usize) -> SourceDeclaration {
        SourceDeclaration::at_path(
            "src/lib.rs",
            format!("declaration_{index}"),
            "function",
            u32::try_from(index).unwrap_or(0).saturating_add(1),
            format!("fn declaration_{index}(argument: u64) -> u64"),
            "x".repeat(512),
        )
        .expect("declaration")
        .with_source_excerpt(
            SourceExcerpt::captured(&"y".repeat(1024), SourceExcerptExtent::Complete)
                .expect("excerpt"),
        )
    }

    fn encoded(record: &ProductSourceRecord) -> usize {
        let mut bytes = Vec::new();
        ProductSourceRelation::encode_value(record, &mut bytes);
        bytes.len()
    }

    #[test]
    fn an_oversized_file_record_is_refused_by_the_unchecked_constructor() {
        // A producer must not be able to build a row the relation cannot
        // store. Before this check the record was accepted here and rejected
        // much later, when the workspace delta was prepared, as an opaque
        // `workspace relation node was rejected`.
        let declarations = (0..64).map(wide_declaration).collect::<Vec<_>>();
        let error = ProductSourceRecord::file(
            [3; 32],
            "src/lib.rs",
            super::SourceLanguage::Rust,
            [4; 32],
            [5; 32],
            Arc::from(declarations),
        )
        .expect_err("an oversized row must be refused");
        assert!(
            error.contains("canonical row capacity"),
            "the refusal must name the capacity it violated, got {error}"
        );
        assert!(
            error.contains("src/lib.rs"),
            "the refusal must name the file, got {error}"
        );
    }

    #[test]
    fn shedding_fits_the_row_and_states_exactly_what_it_dropped() {
        let declarations = (0..64).map(wide_declaration).collect::<Vec<_>>();
        let record = ProductSourceRecord::file_within_row_capacity(
            [3; 32],
            "src/lib.rs",
            super::SourceLanguage::Rust,
            [4; 32],
            [5; 32],
            Arc::from(declarations.clone()),
        )
        .expect("a capacity-aware record");
        let fields = record.file_fields().expect("file fields");

        assert!(encoded(&record) <= ProductSourceRecord::ROW_VALUE_CAPACITY);
        assert_eq!(fields.retention, DeclarationRetention::ExcerptsElided);
        assert_eq!(
            fields.declarations.len(),
            declarations.len(),
            "shedding excerpts must not drop declarations"
        );
        assert_eq!(fields.declarations[0].name(), "declaration_0");
        assert_eq!(
            fields.declarations[0].signature(),
            "fn declaration_0(argument: u64) -> u64",
            "an excerpt-elided row must keep its signatures"
        );
        assert!(
            matches!(
                fields.declarations[0].source_excerpt(),
                SourceExcerpt::NotHydrated
            ),
            "a dropped excerpt must be the typed not-hydrated marker, not empty text"
        );
    }

    #[test]
    fn shedding_reaches_names_only_and_then_truncates_deterministically() {
        let declarations = (0..2048).map(wide_declaration).collect::<Vec<_>>();
        let record = ProductSourceRecord::file_within_row_capacity(
            [3; 32],
            "src/lib.rs",
            super::SourceLanguage::Rust,
            [4; 32],
            [5; 32],
            Arc::from(declarations.clone()),
        )
        .expect("a capacity-aware record");
        let fields = record.file_fields().expect("file fields");

        assert!(encoded(&record) <= ProductSourceRecord::ROW_VALUE_CAPACITY);
        let DeclarationRetention::Truncated(counts) = fields.retention else {
            panic!("expected a truncated row, got {}", fields.retention);
        };
        assert_eq!(
            usize::try_from(counts.extracted()).unwrap_or(0),
            declarations.len(),
            "a truncated row must still report how many declarations existed"
        );
        assert!(
            counts.retained() < counts.extracted(),
            "a truncated row must retain fewer than it extracted"
        );
        assert_eq!(
            usize::try_from(counts.retained()).unwrap_or(0),
            fields.declarations.len(),
            "the reported count must match the rows actually carried"
        );

        let again = ProductSourceRecord::file_within_row_capacity(
            [3; 32],
            "src/lib.rs",
            super::SourceLanguage::Rust,
            [4; 32],
            [5; 32],
            Arc::from(declarations),
        )
        .expect("a capacity-aware record");
        assert_eq!(record, again, "shedding must be a pure function");
    }

    #[test]
    fn every_retention_claim_round_trips_through_the_wire() {
        let claims = [
            DeclarationRetention::Complete,
            DeclarationRetention::ExcerptsElided,
            DeclarationRetention::NamesOnly,
            DeclarationRetention::Truncated(
                RetainedDeclarations::new(3, 9).expect("retention counts"),
            ),
            DeclarationRetention::Unavailable(SourceUnavailableReason::Unreadable),
            DeclarationRetention::Unavailable(SourceUnavailableReason::NotText),
            DeclarationRetention::Unavailable(SourceUnavailableReason::TooLarge),
            DeclarationRetention::Unavailable(SourceUnavailableReason::Unparsed),
        ];
        for claim in claims {
            let record = ProductSourceRecord::file_with_retention(
                [3; 32],
                "src/lib.rs",
                super::SourceLanguage::Rust,
                [4; 32],
                [5; 32],
                Arc::from([wide_declaration(0)]),
                claim,
            )
            .expect("record");
            let mut bytes = Vec::new();
            ProductSourceRelation::encode_value(&record, &mut bytes);
            let decoded = ProductSourceRelation::decode_value(&bytes).expect("decode");
            assert_eq!(
                decoded.file_fields().expect("file fields").retention,
                claim,
                "retention {claim} must survive the wire"
            );
            assert_eq!(decoded, record);
        }
    }

    #[test]
    fn an_unavailable_file_still_names_itself_and_its_reason() {
        let record = ProductSourceRecord::file_unavailable(
            [3; 32],
            "src/binary.rs",
            super::SourceLanguage::Rust,
            [5; 32],
            SourceUnavailableReason::NotText,
        )
        .expect("unavailable record");
        let fields = record.file_fields().expect("file fields");
        assert_eq!(fields.path, "src/binary.rs");
        assert_eq!(
            fields.retention,
            DeclarationRetention::Unavailable(SourceUnavailableReason::NotText)
        );
        assert!(fields.declarations.is_empty());
        assert_eq!(
            fields.content_version, [0; 32],
            "nothing is known about the bytes, so the content version must be zero \
             and a later successful scan must change the row"
        );
    }

    #[test]
    fn a_project_frontier_above_the_row_capacity_names_its_ceiling() {
        let count = u32::try_from(ProductSourceRecord::ROW_VALUE_CAPACITY / 32)
            .unwrap_or(0)
            .saturating_add(2);
        let files = (0..count)
            .map(|index| {
                let mut key = [0; 32];
                key[..4].copy_from_slice(&index.to_be_bytes());
                key
            })
            .collect::<Vec<_>>();
        let Err(error) = ProductSourceRecord::project("project", [1; 32], files) else {
            panic!("a frontier of {count} files must be refused");
        };
        assert!(
            error.contains("canonical row capacity"),
            "the refusal must name the capacity, got {error}"
        );
        assert!(
            error.contains("admits at most"),
            "the refusal must name the per-coordinate file ceiling, got {error}"
        );
        // The guaranteed bound must itself be admissible under the longest
        // label, otherwise the published constant is a lie.
        let files = (0..u32::try_from(ProductSourceRecord::MAX_FRONTIER_FILES).unwrap_or(0))
            .map(|index| {
                let mut key = [0; 32];
                key[..4].copy_from_slice(&index.to_be_bytes());
                key
            })
            .collect::<Vec<_>>();
        assert!(
            ProductSourceRecord::project(
                "l".repeat(ProductSourceRecord::MAX_LABEL_BYTES),
                [1; 32],
                files
            )
            .is_ok(),
            "the published frontier bound must hold for a maximum-length label"
        );
    }
}
