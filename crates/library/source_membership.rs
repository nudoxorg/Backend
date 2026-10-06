//! Snapshot-bound pages of the exact files selected by one indexed Project.

use crate::{MAX_PRODUCT_TEXT_BYTES, MAX_SELECTED_PROJECT_FRONTIER_FILES, PackageReference};
use serde::{Deserialize, Serialize};

/// Maximum number of selected source files returned by one page.
pub const MAX_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES: u16 = 128;

/// Maximum canonical package-relative path size in a membership reply.
pub const MAX_PACKAGE_SOURCE_MEMBERSHIP_PATH_BYTES: usize = MAX_PRODUCT_TEXT_BYTES;

/// Exact package source membership page request schema.
pub const PACKAGE_SOURCE_MEMBERSHIP_SCHEMA: u16 = 1;

/// Exact package-relative members selected by one indexed local Project.
///
/// `content_version` is the `backend_compile::InputContentSchema` v1 object
/// version over the exact source bytes. `source_identity`, when present, is
/// the `backend_version::ContentId<SourceFactDomain>` over those same bytes;
/// that identity uses the `nudox.source.v1\0` BLAKE3 domain tag. A missing
/// source identity remains missing and must not be inferred from the other
/// digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSourceMembershipFileV1 {
    /// Canonical ProductSourceRelation file key.
    pub file_key: [u8; 32],
    /// Canonical slash-separated path relative to the selected Project root.
    pub path: String,
    /// Closed parser-language authority recorded by the selected File row.
    pub language: PackageSourceMembershipLanguageV1,
    /// InputContentSchema v1 digest over the exact source bytes.
    pub content_version: [u8; 32],
    /// Optional SourceFactDomain identity over the exact source bytes.
    pub source_identity: Option<[u8; 32]>,
}

impl PackageSourceMembershipFileV1 {
    /// Checks path canonicality and all fixed-width identities.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        canonical_relative_path(&self.path)
    }
}

/// Closed language recorded on a selected source File row.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageSourceMembershipLanguageV1 {
    /// Rust source.
    Rust,
    /// TypeScript source.
    TypeScript,
    /// Python source.
    Python,
    /// Go source.
    Go,
    /// Java source.
    Java,
    /// C# source.
    CSharp,
    /// C-family source parsed through Clang.
    Clang,
}

impl From<crate::SourceLanguage> for PackageSourceMembershipLanguageV1 {
    fn from(language: crate::SourceLanguage) -> Self {
        match language {
            crate::SourceLanguage::Rust => Self::Rust,
            crate::SourceLanguage::TypeScript => Self::TypeScript,
            crate::SourceLanguage::Python => Self::Python,
            crate::SourceLanguage::Go => Self::Go,
            crate::SourceLanguage::Java => Self::Java,
            crate::SourceLanguage::CSharp => Self::CSharp,
            crate::SourceLanguage::Clang => Self::Clang,
        }
    }
}

/// Scope represented by the selected Project's exact membership rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageSourceMembershipScopeV1 {
    /// Every File row selected by the current indexed Project membership.
    IndexedProjectMembership,
}

/// Whether this Project relation recorded why other source-like files were
/// excluded from its selected membership.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageSourceMembershipExclusionsV1 {
    /// The current Project schema does not retain exclusion reasons such as
    /// tsconfig selections, platform conditions, or Go build tags.
    NotCaptured,
}

/// Stateless continuation bound to one package, selected Project root, and
/// exact last member. Page coordinates let the owner resume from the retained
/// bounded membership page without rebuilding the Project's complete graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSourceMembershipCursorV1 {
    /// Cursor schema.
    pub schema: u16,
    /// Exact local package route owning this cursor.
    pub package: PackageReference,
    /// Stable ProductSourceRelation Project key.
    pub project_key: [u8; 32],
    /// Selected ProductSourceRelation root used by the first page.
    pub source_relation_root: [u8; 32],
    /// Exact content identity recorded by the selected Project row.
    pub source_version: [u8; 32],
    /// Membership page index, or zero for an inline historical frontier.
    pub membership_page: u16,
    /// Index of `last_file_key` within the referenced inline/page membership.
    pub membership_offset: u16,
    /// Zero-based position of `last_file_key` in the complete Project frontier.
    pub ordinal: u32,
    /// Exact canonical file key of the last row returned by the prior page.
    pub last_file_key: [u8; 32],
}

/// Bounded query for the selected source members of one local package.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSourceMembershipPageRequestV1 {
    /// Exact local Project label. Registry packages have no local membership.
    pub package: PackageReference,
    /// Expected relation root; absent only for a first-page request.
    pub expected_source_relation_root: Option<[u8; 32]>,
    /// Expected selected Project source version; paired with the expected root.
    pub expected_source_version: Option<[u8; 32]>,
    /// Continuation copied from the preceding page, when resuming.
    pub cursor: Option<PackageSourceMembershipCursorV1>,
    /// Requested page size, at most [`MAX_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES`].
    pub limit: u16,
}

impl PackageSourceMembershipPageRequestV1 {
    /// Creates a first-page request for one local package.
    #[must_use]
    pub fn first(package: PackageReference) -> Self {
        Self {
            package,
            expected_source_relation_root: None,
            expected_source_version: None,
            cursor: None,
            limit: MAX_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES,
        }
    }

    /// Binds this request to the selected frontier returned by a prior query.
    #[must_use]
    pub fn with_selection(
        mut self,
        source_relation_root: [u8; 32],
        source_version: [u8; 32],
    ) -> Self {
        self.expected_source_relation_root = Some(source_relation_root);
        self.expected_source_version = Some(source_version);
        self
    }

    /// Adds a continuation from the prior membership page.
    #[must_use]
    pub fn with_cursor(mut self, cursor: PackageSourceMembershipCursorV1) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Validates the request before it enters a transport or owner.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        let PackageReference::Local(label) = &self.package else {
            return false;
        };
        let has_root = self.expected_source_relation_root.is_some();
        let has_version = self.expected_source_version.is_some();
        if has_root != has_version
            || self.limit == 0
            || self.limit > MAX_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES
            || label.as_str().len() > MAX_PRODUCT_TEXT_BYTES
        {
            return false;
        }
        match &self.cursor {
            None => true,
            Some(cursor) => {
                has_root
                    && cursor.schema == PACKAGE_SOURCE_MEMBERSHIP_SCHEMA
                    && cursor.package == self.package
                    && cursor.source_relation_root
                        == self.expected_source_relation_root.unwrap_or([0; 32])
                    && cursor.source_version == self.expected_source_version.unwrap_or([0; 32])
                    && cursor.ordinal < MAX_SELECTED_PROJECT_FRONTIER_FILES as u32
            }
        }
    }
}

/// Typed result of reading exact selected source membership.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PackageSourceMembershipPageResultV1 {
    /// One bounded page from the exact selected Project membership.
    Page {
        /// Exact local package route answered.
        package: PackageReference,
        /// Project key committing the selected member relation.
        project_key: [u8; 32],
        /// Selected ProductSourceRelation root for every page in this sequence.
        source_relation_root: [u8; 32],
        /// Content identity recorded by the selected Project row.
        source_version: [u8; 32],
        /// Exact total number of File members selected by the Project.
        file_count: u32,
        /// Number of selected members preceding the first row in `files`.
        start_offset: u32,
        /// Scope represented by these exact File members.
        scope: PackageSourceMembershipScopeV1,
        /// Exclusion evidence recorded by the current Project schema.
        exclusions: PackageSourceMembershipExclusionsV1,
        /// Source-file members in canonical relation-key order.
        files: Box<[PackageSourceMembershipFileV1]>,
        /// Continuation for the next bounded page, or `None` at the end.
        next: Option<PackageSourceMembershipCursorV1>,
    },
    /// The expected selected root/version no longer matches the owner snapshot.
    Stale {
        /// Exact local package route requested.
        package: PackageReference,
        /// Current relation root when the owner has one.
        current_source_relation_root: Option<[u8; 32]>,
        /// Current Project source version when the package remains selected.
        current_source_version: Option<[u8; 32]>,
    },
    /// No exact local Project membership page could be returned.
    Unavailable {
        /// Exact local package route requested.
        package: PackageReference,
        /// Stable reason the selected member set could not be read.
        reason: PackageSourceMembershipUnavailableV1,
    },
}

impl PackageSourceMembershipPageResultV1 {
    /// Checks that the owner reply is bounded and self-consistent.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        match self {
            Self::Page {
                package,
                project_key,
                source_relation_root,
                source_version,
                file_count,
                start_offset,
                scope: PackageSourceMembershipScopeV1::IndexedProjectMembership,
                exclusions: PackageSourceMembershipExclusionsV1::NotCaptured,
                files,
                next,
            } => {
                matches!(package, PackageReference::Local(_))
                    && *file_count as usize <= MAX_SELECTED_PROJECT_FRONTIER_FILES
                    && (*start_offset as usize) <= *file_count as usize
                    && (files.is_empty() == (*file_count == 0))
                    && files.len() <= MAX_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES as usize
                    && files
                        .iter()
                        .all(PackageSourceMembershipFileV1::has_admissible_shape)
                    && files
                        .windows(2)
                        .all(|pair| pair[0].file_key < pair[1].file_key)
                    && (*start_offset as usize).saturating_add(files.len()) <= *file_count as usize
                    && next.is_some()
                        == ((*start_offset as usize).saturating_add(files.len())
                            < *file_count as usize)
                    && next.as_ref().is_none_or(|cursor| {
                        files.last().is_some_and(|last| {
                            cursor.schema == PACKAGE_SOURCE_MEMBERSHIP_SCHEMA
                                && cursor.package == *package
                                && cursor.project_key == *project_key
                                && cursor.source_relation_root == *source_relation_root
                                && cursor.source_version == *source_version
                                && cursor.last_file_key == last.file_key
                                && cursor.ordinal
                                    == start_offset
                                        .saturating_add(files.len() as u32)
                                        .saturating_sub(1)
                        })
                    })
            }
            Self::Stale {
                package,
                current_source_relation_root,
                current_source_version,
            } => {
                matches!(package, PackageReference::Local(_))
                    && (current_source_version.is_none() || current_source_relation_root.is_some())
            }
            Self::Unavailable { package, .. } => matches!(package, PackageReference::Local(_)),
        }
    }
}

/// Stable reason a selected package membership query could not answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageSourceMembershipUnavailableV1 {
    /// The local package has no selected Project row.
    ProjectNotSelected,
    /// A membership or source File row is missing or inconsistent.
    SelectedRelationInvalid,
}

fn canonical_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PACKAGE_SOURCE_MEMBERSHIP_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}
