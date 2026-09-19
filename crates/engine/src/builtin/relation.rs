//! Typed source relations and owner-held lazy source snapshots.

use super::complete_coverage;
use crate::workspace::{TransitionWork, WorkspaceRelationHandle, WorkspaceSnapshot};
pub use backend_compile::{
    Container, DeclarationKind, SourceDeclaration, SourceLanguage, SourceLocation,
};
use backend_compile::{SourceExcerpt, SourceExcerptExtent};
use backend_execution::AuthorityVersion;
use backend_replication::ImmutableObjectSchema;
use backend_version::{
    CanonicalRelation, ContentId, CoverageWitness, DEFAULT_CUT_POLICY, Relation, RelationState,
    SourceFactDomain, StateRoot, WorkspaceRoot,
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

/// Newest source-record format version, as the tags above name it.
const SOURCE_RECORD_VERSION: u8 = 10;

/// Returns the lowest format tag that can carry this record.
///
/// A persisted intent embeds these exact bytes, and restart admission
/// decodes and re-encodes them and refuses anything not byte-identical. A
/// record with nothing new to say must therefore still encode exactly the
/// way it always did, or every workspace written before containment existed
/// would stop opening. The tag is minimal for that reason, the way a
/// canonical integer takes the shortest form: `PSR9` means precisely "at
/// least one declaration states where it sits", and `PSRA` means "the row
/// also states which `SourceFactDomain` content identity its exact bytes
/// hash to".
///
/// Decoding stays liberal - it admits a `PSR9` record that states no
/// containment and normalizes it to `PSR8` on the way out - because refusing
/// a record whose bytes are perfectly readable would make a whole workspace
/// unopenable over a tag.
fn source_record_format(value: &ProductSourceRecord) -> &'static [u8; 4] {
    match value {
        ProductSourceRecord::Project { .. } => SOURCE_RECORD_FORMAT_PLAIN,
        ProductSourceRecord::File {
            declarations,
            source_identity,
            ..
        } => {
            if source_identity.is_some() {
                SOURCE_RECORD_FORMAT_IDENTIFIED
            } else if states_containment(declarations) {
                SOURCE_RECORD_FORMAT_CONTAINED
            } else {
                SOURCE_RECORD_FORMAT_PLAIN
            }
        }
    }
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
    // `PSRA` is the identified format: the eleventh shape, named by a letter
    // because a tenth digit would read as "1" followed by nothing.
    let version = match tag {
        b'A' => 10,
        digit => digit.checked_sub(b'0')?,
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
    /// Maximum source files retained by one project record.
    pub const MAX_PROJECT_FILES: usize = 100_000;
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
    /// File frontier every project row can name, whatever its label.
    ///
    /// A project row is its label plus 32 bytes for every selected file, so
    /// the canonical row capacity bounds the frontier far below any logical
    /// declaration limit.  This constant is the bound that holds even for a
    /// maximum-length label; a short label admits somewhat more, and the
    /// constructor reports that exact per-label ceiling.  Projects above it
    /// need a paged frontier, which does not exist yet: until it does, the
    /// constructor states the ceiling instead of deferring an opaque tree
    /// rejection.
    pub const MAX_FRONTIER_FILES: usize =
        (Self::ROW_VALUE_CAPACITY - Self::MAX_LABEL_BYTES - 64) / 32;

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
            files: Arc::from(files.into_boxed_slice()),
        };
        let encoded = record.encoded_value_bytes();
        if encoded > Self::ROW_VALUE_CAPACITY {
            let named = record
                .project_fields()
                .map_or(0, |fields| fields.files.len());
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
            Self::Project { .. } => None,
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
            Self::Project { .. } => Err(
                "a project record names no source bytes, so it carries no content identity"
                    .to_owned(),
            ),
        }
    }
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
            } => {
                output.push(1);
                push_text(output, label);
                output.extend_from_slice(source_version);
                push_count(output, files.len());
                for file in files.iter() {
                    output.extend_from_slice(file);
                }
            }
            ProductSourceRecord::File { .. } => encode_file_record(value, output),
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
    let identified = source_identity.is_some();
    debug_assert_eq!(
        identified,
        source_record_format(value) == SOURCE_RECORD_FORMAT_IDENTIFIED,
        "the minimal format tag must name the identity exactly when one is stated"
    );
    let contained = source_record_format(value) == SOURCE_RECORD_FORMAT_CONTAINED;
    output.push(2);
    output.extend_from_slice(project);
    push_text(output, path);
    output.push(language.wire_tag());
    output.extend_from_slice(content_version);
    output.extend_from_slice(analysis_version);
    if identified {
        output.extend_from_slice(source_identity.as_ref().expect("checked above").as_ref());
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
        if identified {
            // The identified format carries containment unconditionally: its
            // tag already spends the format discriminant on the identity, so
            // containment cannot also select the tag.
            push_container(output, declaration.container());
        } else if contained {
            push_container(output, declaration.container());
        }
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

fn decode_source_record(bytes: &[u8]) -> Result<ProductSourceRecord, ()> {
    let mut reader = SourceReader::new(bytes);
    let version = format_version(reader.take(4)?).ok_or(())?;
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
        2 => decode_file_record(&mut reader, version)?,
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
    let source_identity = if version >= 10 {
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
    // Containment is tag-selected: `PSR9` and `PSRA` both carry it, one per
    // declaration, while every earlier format implies the file module.
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
    Ok(SourceDeclaration::with_location(
        SourceLocation::new(declaration_path, line).map_err(|_| ())?,
        name,
        kind.name(),
        signature,
        documentation,
    )
    .map_err(|_| ())?
    .with_source_excerpt(source_excerpt)
    .with_container(decoded_container))
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
        DeclarationKind, DeclarationRetention, ProductSourceRecord, ProductSourceRelation,
        Relation, RetainedDeclarations, SourceDeclaration, SourceExcerpt, SourceExcerptExtent,
        SourceLocation, SourceUnavailableReason,
    };
    use backend_version::{CanonicalRelation, ContentId, SourceFactDomain};
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
