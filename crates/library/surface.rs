//! Typed commands and results owned by the durable product service.

use crate::{CommandId, RegistryNativeMetadata};
use backend_advisory::{AdvisoryPackageDto, OverrideEvidence};
pub use backend_semantic::vocabulary::{PackageUrl as PackageCoordinate, RegistryEcosystem};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

/// Largest user-authored operand retained by the product service.
pub const MAX_PRODUCT_TEXT_BYTES: usize = 4096;
/// Largest row collection in one product request or reply.
pub const MAX_PRODUCT_ROWS: usize = 256;

/// Nonempty, bounded, NUL-free product text.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProductText(String);

impl ProductText {
    /// Admits one product operand.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when the text is empty, contains NUL, or exceeds the
    /// fixed operand byte bound.
    pub fn new(value: impl Into<String>) -> Result<Self, ProductAdmissionError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ProductAdmissionError::EmptyText);
        }
        if trimmed.len() > MAX_PRODUCT_TEXT_BYTES || trimmed.bytes().any(|byte| byte == 0) {
            return Err(ProductAdmissionError::TextBound);
        }
        if trimmed.len() == value.len() {
            Ok(Self(value))
        } else {
            Ok(Self(trimmed.to_owned()))
        }
    }

    /// Creates text from a compile-time literal that is part of the product
    /// protocol. Callers use this only for fixed protocol reason strings;
    /// user-authored values must continue through [`Self::new`].
    #[must_use]
    pub fn from_static(value: &'static str) -> Self {
        debug_assert!(!value.trim().is_empty());
        debug_assert!(value.len() <= MAX_PRODUCT_TEXT_BYTES);
        debug_assert!(!value.bytes().any(|byte| byte == 0));
        Self(value.to_owned())
    }
    /// Returns the admitted text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProductText {
    type Error = ProductAdmissionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<ProductText> for String {
    fn from(value: ProductText) -> Self {
        value.0
    }
}

/// An admitted pinned purl or local canonical package label.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum PackageReference {
    /// Canonical version-pinned package URL.
    Purl(PackageCoordinate),
    /// Canonical label in the immutable local view.
    Local(ProductText),
}

impl PackageReference {
    /// Parses the package namespace once at the command boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when a package URL is malformed or a local label fails
    /// text admission.
    pub fn parse(value: impl Into<String>) -> Result<Self, ProductAdmissionError> {
        let value = value.into();
        if value.starts_with("pkg:") {
            PackageCoordinate::parse(value)
                .map(Self::Purl)
                .map_err(|_| ProductAdmissionError::PackageReference)
        } else {
            ProductText::new(value).map(Self::Local)
        }
    }
    /// Returns the canonical spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Purl(v) => v.as_str(),
            Self::Local(v) => v.as_str(),
        }
    }
}

/// A bounded unique project name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectName(ProductText);

impl ProjectName {
    /// Admits a project name no longer than 64 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when the name is empty, too long, or contains control
    /// characters.
    pub fn new(value: impl Into<String>) -> Result<Self, ProductAdmissionError> {
        let value = ProductText::new(value)?;
        if value.as_str().len() > 64 || value.as_str().bytes().any(|b| b.is_ascii_control()) {
            return Err(ProductAdmissionError::ProjectName);
        }
        Ok(Self(value))
    }
    /// Returns the admitted name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl TryFrom<String> for ProjectName {
    type Error = ProductAdmissionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<ProjectName> for String {
    fn from(value: ProjectName) -> Self {
        value.0.into()
    }
}

/// Stable, never-reused project identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(NonZeroU64);
impl ProjectId {
    /// Wraps a nonzero identity.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }
    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A project selected by stable identity or unique name.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", content = "value", rename_all = "kebab-case")]
pub enum ProjectSelector {
    /// Stable identity.
    Id(ProjectId),
    /// Unique name.
    Name(ProjectName),
}
impl ProjectSelector {
    /// Parses digits as an identity and other text as a name.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when a nonnumeric selector is not an admitted project
    /// name.
    pub fn parse(value: &str) -> Result<Self, ProductAdmissionError> {
        value
            .parse::<u64>()
            .ok()
            .and_then(NonZeroU64::new)
            .map(ProjectId::new)
            .map_or_else(
                || ProjectName::new(value).map(Self::Name),
                |id| Ok(Self::Id(id)),
            )
    }
}

/// Stable session-tree node identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TreeNodeId(NonZeroU64);
impl TreeNodeId {
    /// Wraps a nonzero node identity.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }
    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Which product surface opened a session node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "surface", content = "client", rename_all = "kebab-case")]
pub enum TreeOpener {
    /// Desktop application.
    Desktop,
    /// Command line.
    Cli,
    /// MCP client with its admitted name.
    Mcp(ProductText),
}

/// What one shared session node displays.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subject", content = "value", rename_all = "kebab-case")]
pub enum TreeSubject {
    /// Local or registry package.
    Package(PackageReference),
    /// Declaration label.
    Declaration(ProductText),
    /// Registry exploration query.
    Explore(Option<ProductText>),
    /// Documentation search query.
    Search(ProductText),
    /// Registry publisher handle.
    Owner(ProductText),
}

/// Canonical two-byte compiler language profile used by semantic history.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SemanticLanguageProfile([u8; 2]);

impl SemanticLanguageProfile {
    /// Encodes one closed compiler language profile.
    #[must_use]
    pub fn new(profile: backend_semantic::vocabulary::LanguageProfile) -> Self {
        Self(profile.into())
    }

    /// Reopens the closed compiler language profile.
    ///
    /// # Errors
    /// Returns a product admission error for an unknown language/profile pair.
    pub fn profile(
        self,
    ) -> Result<backend_semantic::vocabulary::LanguageProfile, ProductAdmissionError> {
        backend_semantic::vocabulary::LanguageProfile::try_from(self.0)
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)
    }

    /// Returns the canonical profile bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 2] {
        self.0
    }

    /// Admits one closed profile by the spelling every surface uses.
    ///
    /// The nine product profiles are the closed set the built-in semantic
    /// plane advertises; a surface that accepts free text here would let a
    /// caller ask for a language the engine cannot answer for.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        backend_semantic::vocabulary::LanguageProfile::PRODUCT_PROFILES
            .into_iter()
            .map(Self::new)
            .find(|profile| profile.name() == Some(name))
    }

    /// Returns the closed profile spelling shared by every surface.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};
        match self.profile().ok()? {
            LanguageProfile::Rust(_) => Some("rust"),
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript) => Some("typescript"),
            LanguageProfile::TypeScript(TypeScriptSource::Tsx) => Some("tsx"),
            LanguageProfile::Python(_) => Some("python"),
            LanguageProfile::Go(_) => Some("go"),
            LanguageProfile::Java(_) => Some("java"),
            LanguageProfile::CSharp(_) => Some("csharp"),
            LanguageProfile::C(_) => Some("c"),
            LanguageProfile::Cxx(_) => Some("cpp"),
        }
    }

    /// Returns every closed profile spelling, in product order.
    #[must_use]
    pub fn names() -> Vec<&'static str> {
        backend_semantic::vocabulary::LanguageProfile::PRODUCT_PROFILES
            .into_iter()
            .map(Self::new)
            .filter_map(Self::name)
            .collect()
    }
}

/// Exact immutable compiler generation binding exposed to product clients.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SemanticGenerationId([u8; 32]);

impl SemanticGenerationId {
    /// Creates a product identity from an admitted compiler binding identity.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the exact compiler binding identity bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// One immutable compiler publication available for exact selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticVersionRecord {
    /// Product package whose semantic plane owns this generation.
    pub package: PackageReference,
    /// Exact compiler package URL, including version, qualifiers, and subpath.
    pub coordinate: PackageCoordinate,
    /// Closed source language and dialect profile.
    pub profile: SemanticLanguageProfile,
    /// Exact immutable compilation binding.
    pub generation: SemanticGenerationId,
    /// Verified generation root selected by the compiler journal.
    pub generation_root: [u8; 32],
    /// Verified dependency-set commitment selected by the compiler journal.
    pub dependency_set: [u8; 32],
    /// Semantic manifest committed by the generation binding.
    pub manifest: [u8; 32],
    /// Number of semantic image artifacts in the manifest.
    pub artifacts: u32,
    /// Canonical semantic image bytes covered by the manifest.
    pub semantic_bytes: u32,
    /// Whether the compiler authority covered the complete declared scope.
    pub complete: bool,
    /// Whether the mutable target row currently selects this generation.
    pub selected: bool,
}

impl SemanticVersionRecord {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        self.profile.profile()?;
        if self.coordinate.package_type().language() != self.profile.profile()?.language()
            || self.artifacts == 0
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
}

/// Exact request for one daemon-owned product operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SurfaceCommand {
    /// Read the complete advisory decision for one exact package version.
    Advisory {
        /// Package locator.
        package: PackageReference,
        /// Optional checked override for a blocked decision.
        override_evidence: Option<OverrideEvidence>,
    },
    /// Read several declarations independently.
    Read {
        /// Canonical declaration labels.
        locators: Box<[ProductText]>,
    },
    /// Find the source-verified sites where one declaration is used.
    References {
        /// Exact declaration coordinate whose uses are requested.
        target: ProductText,
    },
    /// Compare declaration sets between package versions.
    Diff {
        /// Older package.
        from: PackageReference,
        /// Newer package.
        to: PackageReference,
    },
    /// Browse the local registry catalog.
    Explore {
        /// Optional case-insensitive filter.
        query: Option<ProductText>,
        /// Bounded page size.
        limit: u16,
    },
    /// Read one local registry package.
    Package {
        /// Package locator.
        package: PackageReference,
    },
    /// Acquire a project or package directly from a pinned code-forge source.
    ForgeAdd {
        /// Canonical forge URL with an explicit tag, branch, or commit.
        coordinate: ProductText,
    },
    /// Reference an already acquired code-forge source from the local cache.
    ForgeReference {
        /// Canonical forge URL with an explicit tag, branch, or commit.
        coordinate: ProductText,
    },
    /// Read reverse dependency metadata.
    Dependents {
        /// Package locator.
        package: PackageReference,
    },
    /// Read outgoing dependency metadata.
    Dependencies {
        /// Package locator.
        package: PackageReference,
    },
    /// Read packages associated with one publisher.
    Owner {
        /// Registry publisher handle.
        owner: ProductText,
    },
    /// Search the local registry catalog.
    IndexSearch {
        /// Search query.
        query: ProductText,
        /// Bounded page size.
        limit: u16,
    },
    /// Read versions recorded under one package name.
    PackageVersions {
        /// Package locator.
        package: PackageReference,
    },
    /// Read immutable compiler generations retained for one package.
    SemanticVersions {
        /// Exact product package reference.
        package: PackageReference,
    },
    /// Select one exact retained compiler generation for product projection.
    SelectSemanticVersion {
        /// Exact product package reference.
        package: PackageReference,
        /// Exact compiler coordinate for the target language lane.
        coordinate: PackageCoordinate,
        /// Closed language and dialect profile.
        profile: SemanticLanguageProfile,
        /// Immutable binding identity to select.
        generation: SemanticGenerationId,
    },
    /// Read the newest local package plus version count.
    PackageProfile {
        /// Package locator.
        package: PackageReference,
    },
    /// Follow one package for releases.
    Subscribe {
        /// Package locator.
        package: PackageReference,
        /// Optional project folder.
        project: Option<ProjectSelector>,
    },
    /// Stop following one package.
    Unsubscribe {
        /// Package locator.
        package: PackageReference,
    },
    /// List subscriptions.
    Subscriptions,
    /// Read followed-package releases.
    Releases {
        /// Mark returned newest releases as seen.
        mark_seen: bool,
    },
    /// List projects.
    Projects,
    /// Create a project.
    ProjectCreate {
        /// Unique name.
        name: ProjectName,
        /// Optional absolute supported lockfile path.
        lockfile: Option<ProductText>,
    },
    /// Delete a project.
    ProjectDelete {
        /// Project selector.
        project: ProjectSelector,
    },
    /// Add one project member.
    ProjectAdd {
        /// Project selector.
        project: ProjectSelector,
        /// Pinned package.
        package: PackageReference,
    },
    /// Remove one project member.
    ProjectRemove {
        /// Project selector.
        project: ProjectSelector,
        /// Pinned package.
        package: PackageReference,
    },
    /// Reconcile from the bound lockfile.
    ProjectSync {
        /// Project selector.
        project: ProjectSelector,
    },
    /// Read the shared session tree.
    Tree,
    /// Open or focus a tree subject.
    TreeOpen {
        /// Subject to open.
        subject: TreeSubject,
        /// Optional parent.
        parent: Option<TreeNodeId>,
        /// Optional title.
        title: Option<ProductText>,
        /// Calling surface.
        opener: TreeOpener,
    },
    /// Close a node or branch.
    TreeClose {
        /// Node to close.
        node: TreeNodeId,
        /// Include descendants.
        branch: bool,
    },
}

impl SurfaceCommand {
    /// Returns this command's exact registry identity.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        match self {
            Self::Advisory { .. } => CommandId::Advisory,
            Self::Read { .. } => CommandId::Read,
            Self::References { .. } => CommandId::References,
            Self::Diff { .. } => CommandId::Diff,
            Self::Explore { .. } => CommandId::Explore,
            Self::Package { .. } => CommandId::Package,
            Self::ForgeAdd { .. } => CommandId::ForgeAdd,
            Self::ForgeReference { .. } => CommandId::ForgeReference,
            Self::Dependents { .. } => CommandId::Dependents,
            Self::Dependencies { .. } => CommandId::Dependencies,
            Self::Owner { .. } => CommandId::Owner,
            Self::IndexSearch { .. } => CommandId::IndexSearch,
            Self::PackageVersions { .. } => CommandId::PackageVersions,
            Self::SemanticVersions { .. } => CommandId::SemanticVersions,
            Self::SelectSemanticVersion { .. } => CommandId::SelectSemanticVersion,
            Self::PackageProfile { .. } => CommandId::PackageProfile,
            Self::Subscribe { .. } => CommandId::Subscribe,
            Self::Unsubscribe { .. } => CommandId::Unsubscribe,
            Self::Subscriptions => CommandId::Subscriptions,
            Self::Releases { .. } => CommandId::Releases,
            Self::Projects => CommandId::Projects,
            Self::ProjectCreate { .. } => CommandId::ProjectCreate,
            Self::ProjectDelete { .. } => CommandId::ProjectDelete,
            Self::ProjectAdd { .. } => CommandId::ProjectAdd,
            Self::ProjectRemove { .. } => CommandId::ProjectRemove,
            Self::ProjectSync { .. } => CommandId::ProjectSync,
            Self::Tree => CommandId::Tree,
            Self::TreeOpen { .. } => CommandId::TreeOpen,
            Self::TreeClose { .. } => CommandId::TreeClose,
        }
    }
    /// Verifies collection and page bounds after syntactic decoding.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when a request collection or page size violates the
    /// fixed product row bound.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        match self {
            Self::Read { locators } if locators.len() > MAX_PRODUCT_ROWS => {
                Err(ProductAdmissionError::RowBound)
            }
            Self::Explore { limit, .. } | Self::IndexSearch { limit, .. }
                if *limit == 0 || usize::from(*limit) > MAX_PRODUCT_ROWS =>
            {
                Err(ProductAdmissionError::RowBound)
            }
            Self::SelectSemanticVersion {
                coordinate,
                profile,
                ..
            } if coordinate.package_type().language() != profile.profile()?.language() => {
                Err(ProductAdmissionError::SemanticVersionShape)
            }
            _ => Ok(()),
        }
    }
}

/// Compact declaration result used by batched reads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationRecord {
    /// Requested label.
    pub label: ProductText,
    /// Stable row digest bytes when found.
    pub stable_id: Option<[u8; 32]>,
    /// Canonical signature when captured.
    pub signature: Option<ProductText>,
}

/// How a declaration changed between package versions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeclarationChange {
    /// Present only in the newer package.
    Added,
    /// Present only in the older package.
    Removed,
    /// Present in both with different signatures.
    Changed,
    /// The family remains present, but overload identity churn prevents a
    /// sound one-to-one pairing.
    Indeterminate,
}

/// Exact stable identity of one semantic declaration instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticDeclarationIdentity {
    /// Stable declaration family.
    pub family: [u8; 16],
    /// Structural fingerprint of this family member.
    pub variant: [u8; 16],
}

/// Closed semantic relation vocabulary emitted by compiler authorities.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticLinkKind {
    /// Direct callable invocation.
    Calls,
    /// Dynamically or statically dispatched method invocation.
    MethodCall,
    /// Use of a declaration as a type.
    TypeReference,
    /// Value read.
    Reads,
    /// Value write.
    Writes,
    /// Module or package import.
    Imports,
    /// Interface or trait implementation.
    Implements,
    /// Member override.
    Overrides,
    /// Public re-export.
    Reexports,
    /// Class or type inheritance.
    Inherits,
    /// Documentation reference.
    Documents,
}

/// Strength of the authority evidence for one semantic relation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticConfidence {
    /// Parsed syntax proves only the written relationship.
    Syntactic,
    /// A language adapter inferred the relationship without full authority.
    Heuristic,
    /// A language index resolved the relationship.
    Indexed,
    /// An imported semantic artifact resolved the relationship.
    Imported,
    /// The native compiler or oracle resolved the relationship.
    Compiler,
}

/// Exact endpoint of a semantic graph relation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SemanticLinkTarget {
    /// Declaration in the same package image.
    Local {
        /// Exact local declaration identity.
        declaration: SemanticDeclarationIdentity,
    },
    /// Declaration in another immutable semantic fragment.
    Stable {
        /// Immutable target fragment identity.
        fragment: [u8; 32],
        /// Exact target declaration identity.
        declaration: SemanticDeclarationIdentity,
    },
    /// Unresolved declaration retained under its foreign authority key.
    Foreign {
        /// Stable foreign declaration family.
        declaration: [u8; 16],
        /// Structural variant when the authority could recover it.
        variant: Option<[u8; 16]>,
    },
    /// Exact entity ordinal in another immutable fragment.
    FragmentEntity {
        /// Immutable target fragment identity.
        fragment: [u8; 32],
        /// Target entity ordinal within that fragment.
        ordinal: u32,
    },
}

/// Captured source site attached to semantic relation evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceSpan {
    /// Canonical package-relative source path.
    pub file: ProductText,
    /// Inclusive UTF-8 byte start.
    pub start: u32,
    /// Exclusive UTF-8 byte end.
    pub end: u32,
}

/// Authority evidence retained for one canonical semantic relation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticLinkEvidence {
    /// Strongest authority classification observed for this relation.
    pub confidence: SemanticConfidence,
    /// Representative source site, when the authority captured one.
    pub source: Option<SemanticSourceSpan>,
}

/// One exact graph relation delta attached to its source declaration.
///
/// Evidence presence is encoded by the variant, so an added link cannot carry
/// older evidence and an evidence change cannot omit either observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SemanticLinkDelta {
    /// Relation present only in the newer generation.
    Added {
        /// Exact source declaration.
        from: SemanticDeclarationIdentity,
        /// Exact local or external endpoint.
        target: SemanticLinkTarget,
        /// Compiler-defined relation kind.
        relation: SemanticLinkKind,
        /// Newer authority evidence.
        evidence: SemanticLinkEvidence,
    },
    /// Relation present only in the older generation.
    Removed {
        /// Exact source declaration.
        from: SemanticDeclarationIdentity,
        /// Exact local or external endpoint.
        target: SemanticLinkTarget,
        /// Compiler-defined relation kind.
        relation: SemanticLinkKind,
        /// Older authority evidence.
        evidence: SemanticLinkEvidence,
    },
    /// Stable relation whose representative authority evidence changed.
    EvidenceChanged {
        /// Exact source declaration.
        from: SemanticDeclarationIdentity,
        /// Exact local or external endpoint.
        target: SemanticLinkTarget,
        /// Compiler-defined relation kind.
        relation: SemanticLinkKind,
        /// Older authority evidence.
        before: SemanticLinkEvidence,
        /// Newer authority evidence.
        after: SemanticLinkEvidence,
    },
}

impl SemanticLinkDelta {
    /// Returns the exact declaration that owns this outgoing relation.
    #[must_use]
    pub const fn source_declaration(&self) -> SemanticDeclarationIdentity {
        match self {
            Self::Added { from, .. }
            | Self::Removed { from, .. }
            | Self::EvidenceChanged { from, .. } => *from,
        }
    }

    fn matches_declaration_sides(
        &self,
        before: Option<SemanticDeclarationIdentity>,
        after: Option<SemanticDeclarationIdentity>,
    ) -> bool {
        match self {
            Self::Added { from, .. } => Some(*from) == after,
            Self::Removed { from, .. } => Some(*from) == before,
            Self::EvidenceChanged {
                from,
                before: older,
                after: newer,
                ..
            } => Some(*from) == before && Some(*from) == after && older != newer,
        }
    }
}

/// One source-verified use of a declaration, as answered by a references
/// query.
///
/// Every record is provenance-bearing by construction: the authority
/// classification and the captured source site ride in `evidence`, the exact
/// endpoint resolution rides in `target`, and `site` names the declaration
/// whose source contains the use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRecord {
    /// Canonical coordinate of the declaration that uses the target.
    pub site: ProductText,
    /// Exact local or foreign endpoint this occurrence resolves to.
    pub target: SemanticLinkTarget,
    /// Compiler-defined relation that makes this site a use.
    pub relation: SemanticLinkKind,
    /// Authority classification and captured source span of the use.
    pub evidence: SemanticLinkEvidence,
}

/// One declaration and graph difference between package generations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffRecord {
    /// Declaration label.
    pub label: ProductText,
    /// Exact change.
    pub change: DeclarationChange,
    /// Exact older declaration identity, when one existed.
    pub before: Option<SemanticDeclarationIdentity>,
    /// Exact newer declaration identity, when one exists.
    pub after: Option<SemanticDeclarationIdentity>,
    /// Bounded graph relation changes sourced by this declaration.
    pub links: Box<[SemanticLinkDelta]>,
}

impl DiffRecord {
    const fn has_valid_identity_shape(&self) -> bool {
        match self.change {
            DeclarationChange::Added => self.before.is_none() && self.after.is_some(),
            DeclarationChange::Removed => self.before.is_some() && self.after.is_none(),
            DeclarationChange::Changed => self.before.is_some() && self.after.is_some(),
            DeclarationChange::Indeterminate => self.before.is_some() ^ self.after.is_some(),
        }
    }
}

/// One locally committed registry publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackageRecord {
    /// Canonical version-pinned purl.
    pub coordinate: PackageReference,
    /// Closed ecosystem spelling.
    pub ecosystem: RegistryEcosystem,
    /// Registry-native package name.
    pub name: ProductText,
    /// Immutable version.
    pub version: ProductText,
    /// Verified archive byte count.
    pub bytes: u64,
    /// Current registry selection policy for this immutable release.
    pub standing: RegistryReleaseStanding,
    /// Latest download observation, with missing data kept distinct from zero.
    pub downloads: RegistryDownloadCount,
    /// Content identity of the registry fact groups above.
    pub facts_version: [u8; 32],
    /// Identity of the versioned native metadata DTO.
    pub native_metadata_version: [u8; 32],
    /// Complete bounded native registry metadata for this release.
    pub native_metadata: RegistryNativeMetadata,
    /// Complete typed advisory evidence and acquisition decision for this version.
    pub advisory: AdvisoryPackageDto,
}

/// A bounded fact whose absence is preserved as a typed state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum ForgeFact<T> {
    /// The configured forge authority reported this value.
    Recorded(T),
    /// The authority did not report this value or it was unreachable.
    Unavailable(ProductText),
}

/// Typed repository metadata shown on a forge-backed package page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeRepositoryMetadataRecord {
    /// Repository owner or namespace.
    pub owner: ForgeFact<ProductText>,
    /// Description, when reported.
    pub description: ForgeFact<ProductText>,
    /// SPDX or forge license label, when reported.
    pub license: ForgeFact<ProductText>,
    /// README content or a bounded unavailable reason.
    pub readme: ForgeFact<ProductText>,
    /// Topics reported by the authority.
    pub topics: ForgeFact<Box<[ProductText]>>,
    /// Stars reported by the authority.
    pub stars: ForgeFact<u64>,
    /// Forks reported by the authority.
    pub forks: ForgeFact<u64>,
}

/// One package manifest discovered in an acquired forge tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeManifestRecord {
    /// Path relative to the selected repository subdirectory.
    pub path: ProductText,
    /// Ecosystem recognized from the manifest.
    pub ecosystem: ProductText,
    /// Immutable package name when the manifest records one.
    pub name: Option<ProductText>,
    /// Immutable package version when the manifest records one.
    pub version: Option<ProductText>,
    /// Number of dependency rows admitted from the shared package graph.
    pub dependency_count: u16,
}

/// Product DTO for a source acquired from GitHub, GitLab, Codeberg, or generic HTTPS Git.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgePackageRecord {
    /// Canonical coordinate as entered by the client.
    pub coordinate: ProductText,
    /// Canonical provider spelling.
    pub provider: ProductText,
    /// Owner/namespace from the coordinate.
    pub owner: ProductText,
    /// Repository name from the coordinate.
    pub repository: ProductText,
    /// Explicit revision spelling.
    pub revision: ProductText,
    /// Optional monorepo subdirectory.
    pub subdir: Option<ProductText>,
    /// Exact resolved commit object, if available.
    pub commit: ForgeFact<ProductText>,
    /// Exact resolved tree object, if the authority reports one.
    pub tree: ForgeFact<ProductText>,
    /// Bounded repository metadata.
    pub metadata: ForgeRepositoryMetadataRecord,
    /// Manifests admitted into the shared dependency graph.
    pub manifests: Box<[ForgeManifestRecord]>,
    /// Archive/tree availability state.
    pub source: ForgeFact<ProductText>,
}

/// Registry policy applied to an immutable release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryReleaseStanding {
    /// Offered to new resolutions.
    Available,
    /// Withdrawn by the publisher.
    Yanked,
    /// Retained with a publisher replacement recommendation.
    Deprecated,
    /// Addressable but hidden from normal listings.
    Unlisted,
    /// Excluded by ecosystem version policy.
    Retracted,
    /// Previously observed and now deleted upstream.
    Removed,
}

/// Download telemetry without conflating absent data with zero downloads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum RegistryDownloadCount {
    /// Exact cumulative release count.
    Exact(u64),
    /// Estimated count.
    Approximate(u64),
    /// Registry exposes no usable count and the source's typed coverage is
    /// carried without encoding meaning in human text.
    Unavailable(RegistryFactAvailability),
}

/// Typed coverage for a registry fact that is absent from a reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryFactAvailability {
    /// The source does not publish the fact.
    NotRecorded,
    /// The source explicitly does not support the fact.
    Unsupported,
    /// The source should provide the fact, but it could not be obtained.
    Unavailable,
    /// The source answered with an older frontier.
    Stale,
    /// The source did not establish coverage.
    Unknown,
}

impl Default for RegistryFactAvailability {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Availability of registry facts not present in every configured feed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum RegistryMetadata<T> {
    /// Facts were recorded.
    Recorded(T),
    /// The configured feed does not publish this fact.
    NotRecorded(ProductText),
}

/// One durable package subscription.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionRecord {
    /// Followed package.
    pub package: PackageReference,
    /// Optional project folder.
    pub project: Option<ProjectId>,
    /// Newest version marked seen.
    pub seen: Option<ProductText>,
}

/// One release from the local registry catalog.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRecord {
    /// Followed package.
    pub package: PackageReference,
    /// Recorded version.
    pub version: ProductText,
    /// Whether this request marked it seen.
    pub seen: bool,
}

/// One durable project folder.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRecord {
    /// Stable identity.
    pub id: ProjectId,
    /// Unique name.
    pub name: ProjectName,
    /// Optional absolute lockfile path.
    pub lockfile: Option<ProductText>,
    /// Pinned members in canonical order.
    pub members: Box<[PackageReference]>,
}

/// One shared session-tree node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeNodeRecord {
    /// Stable identity.
    pub id: TreeNodeId,
    /// Parent node.
    pub parent: Option<TreeNodeId>,
    /// Displayed subject.
    pub subject: TreeSubject,
    /// Short title.
    pub title: ProductText,
    /// Calling surface.
    pub opener: TreeOpener,
    /// Whether this node is active.
    pub active: bool,
}

/// Typed result algebra for durable product commands.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "kebab-case")]
pub enum SurfaceReply {
    /// Complete advisory facts and typed acquisition decision.
    Advisory(AdvisoryPackageDto),
    /// Per-locator declaration results.
    Read(Box<[DeclarationRecord]>),
    /// Source-verified uses of one declaration.
    References {
        /// Requested target coordinate, verbatim.
        target: ProductText,
        /// Bounded use sites, ordered by source position.
        references: Box<[ReferenceRecord]>,
    },
    /// Declaration-level differences.
    Diff(Box<[DiffRecord]>),
    /// Bounded catalog page.
    Explored(Box<[RegistryPackageRecord]>),
    /// Exact package records.
    Package(Box<[RegistryPackageRecord]>),
    /// Result of acquiring a forge source.
    ForgePackageAdded(ForgePackageRecord),
    /// Result of referencing a cached forge source.
    ForgePackageReferenced(ForgePackageRecord),
    /// Reverse dependency facts.
    Dependents(RegistryMetadata<Box<[RegistryPackageRecord]>>),
    /// Outgoing dependency facts.
    Dependencies(crate::DependencyFacts<Box<[crate::PackageDependencyRecord]>>),
    /// Publisher facts.
    Owner(RegistryMetadata<Box<[RegistryPackageRecord]>>),
    /// Bounded local index matches.
    IndexSearch(Box<[RegistryPackageRecord]>),
    /// Recorded versions.
    PackageVersions(Box<[RegistryPackageRecord]>),
    /// Immutable compiler generations for one exact package.
    SemanticVersions(Box<[SemanticVersionRecord]>),
    /// Exact compiler generation selected by the durable owner.
    SemanticVersionSelected(SemanticVersionRecord),
    /// Latest package and history count.
    PackageProfile {
        /// Latest record.
        latest: Option<RegistryPackageRecord>,
        /// Recorded version count.
        versions: u64,
    },
    /// Subscription after mutation.
    Subscribed(SubscriptionRecord),
    /// Whether a subscription was removed.
    Unsubscribed(bool),
    /// Current subscriptions.
    Subscriptions(Box<[SubscriptionRecord]>),
    /// Current release feed.
    Releases(Box<[ReleaseRecord]>),
    /// Current projects.
    Projects(Box<[ProjectRecord]>),
    /// Created project.
    ProjectCreated(ProjectRecord),
    /// Deleted project identity.
    ProjectDeleted(ProjectId),
    /// Project after a member add.
    ProjectAdded(ProjectRecord),
    /// Project after a member removal.
    ProjectRemoved(ProjectRecord),
    /// Project after lockfile reconciliation.
    ProjectSynced(ProjectRecord),
    /// Current shared tree.
    Tree(Box<[TreeNodeRecord]>),
    /// Opened or focused node.
    TreeOpened(TreeNodeRecord),
    /// Number of closed nodes.
    TreeClosed(u64),
}

impl SurfaceReply {
    /// Returns the exact command identity required by this reply.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        match self {
            Self::Advisory(_) => CommandId::Advisory,
            Self::Read(_) => CommandId::Read,
            Self::References { .. } => CommandId::References,
            Self::Diff(_) => CommandId::Diff,
            Self::Explored(_) => CommandId::Explore,
            Self::Package(_) => CommandId::Package,
            Self::ForgePackageAdded(_) => CommandId::ForgeAdd,
            Self::ForgePackageReferenced(_) => CommandId::ForgeReference,
            Self::Dependents(_) => CommandId::Dependents,
            Self::Dependencies(_) => CommandId::Dependencies,
            Self::Owner(_) => CommandId::Owner,
            Self::IndexSearch(_) => CommandId::IndexSearch,
            Self::PackageVersions(_) => CommandId::PackageVersions,
            Self::SemanticVersions(_) => CommandId::SemanticVersions,
            Self::SemanticVersionSelected(_) => CommandId::SelectSemanticVersion,
            Self::PackageProfile { .. } => CommandId::PackageProfile,
            Self::Subscribed(_) => CommandId::Subscribe,
            Self::Unsubscribed(_) => CommandId::Unsubscribe,
            Self::Subscriptions(_) => CommandId::Subscriptions,
            Self::Releases(_) => CommandId::Releases,
            Self::Projects(_) => CommandId::Projects,
            Self::ProjectCreated(_) => CommandId::ProjectCreate,
            Self::ProjectDeleted(_) => CommandId::ProjectDelete,
            Self::ProjectAdded(_) => CommandId::ProjectAdd,
            Self::ProjectRemoved(_) => CommandId::ProjectRemove,
            Self::ProjectSynced(_) => CommandId::ProjectSync,
            Self::Tree(_) => CommandId::Tree,
            Self::TreeOpened(_) => CommandId::TreeOpen,
            Self::TreeClosed(_) => CommandId::TreeClose,
        }
    }
    /// Verifies command identity and reply collection bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when the reply belongs to another command or exceeds the
    /// fixed product row bound.
    #[allow(
        clippy::match_same_arms,
        reason = "distinct typed reply collections share only their length admission rule"
    )]
    pub fn admit(&self, expected: CommandId) -> Result<(), ProductAdmissionError> {
        if self.id() != expected {
            return Err(ProductAdmissionError::CommandMismatch);
        }
        let count = match self {
            Self::Advisory(_) => 1,
            Self::Read(v) => v.len(),
            Self::References { references, .. } => references.len(),
            Self::Diff(rows) => rows.iter().try_fold(rows.len(), |count, row| {
                if !row.has_valid_identity_shape()
                    || row
                        .links
                        .iter()
                        .any(|link| !link.matches_declaration_sides(row.before, row.after))
                {
                    return Err(ProductAdmissionError::DiffShape);
                }
                count
                    .checked_add(row.links.len())
                    .ok_or(ProductAdmissionError::RowBound)
            })?,
            Self::Explored(v)
            | Self::Package(v)
            | Self::IndexSearch(v)
            | Self::PackageVersions(v)
            | Self::Dependents(RegistryMetadata::Recorded(v))
            | Self::Owner(RegistryMetadata::Recorded(v)) => v.len(),
            Self::Dependencies(crate::DependencyFacts::Known(v)) => v.len(),
            Self::SemanticVersions(records) => {
                let mut selected = 0_usize;
                for record in records {
                    record.admit()?;
                    selected = selected.saturating_add(usize::from(record.selected));
                }
                if selected > 1 {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                records.len()
            }
            Self::SemanticVersionSelected(record) => {
                record.admit()?;
                if !record.selected {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                1
            }
            Self::Subscriptions(v) => v.len(),
            Self::Releases(v) => v.len(),
            Self::Projects(v) => v.len(),
            Self::Tree(v) => v.len(),
            _ => 1,
        };
        if count > MAX_PRODUCT_ROWS {
            Err(ProductAdmissionError::RowBound)
        } else {
            let valid_registry_rows = match self {
                Self::Explored(rows)
                | Self::Package(rows)
                | Self::IndexSearch(rows)
                | Self::PackageVersions(rows) => rows.iter().all(valid_registry_record),
                Self::Dependents(RegistryMetadata::Recorded(rows))
                | Self::Owner(RegistryMetadata::Recorded(rows)) => {
                    rows.iter().all(valid_registry_record)
                }
                Self::PackageProfile { latest: Some(row), .. } => valid_registry_record(row),
                _ => true,
            };
            if !valid_registry_rows {
                return Err(ProductAdmissionError::NativeMetadata);
            }
            Ok(())
        }
    }

    pub(crate) fn encoded_size_bound(&self) -> usize {
        const ENVELOPE_BYTES: usize = 512;
        let payload = match self {
            Self::Read(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(declaration_record_bound(record))
            }),
            Self::Advisory(value) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(value).map_or(0, |bytes| bytes.len())),
            Self::References { target, references } => references.iter().fold(
                fixed_record_bound().saturating_add(text_bound(target)),
                |bound, record| {
                    bound
                        .saturating_add(fixed_record_bound())
                        .saturating_add(text_bound(&record.site))
                        .saturating_add(semantic_link_evidence_bound(&record.evidence))
                },
            ),
            Self::Diff(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(diff_record_bound(record))
            }),
            Self::Explored(records)
            | Self::Package(records)
            | Self::IndexSearch(records)
            | Self::PackageVersions(records)
            | Self::Dependents(RegistryMetadata::Recorded(records))
            | Self::Owner(RegistryMetadata::Recorded(records)) => registry_records_bound(records),
            Self::ForgePackageAdded(record) | Self::ForgePackageReferenced(record) => {
                fixed_record_bound()
                    .saturating_add(serde_json::to_vec(record).map_or(0, |bytes| bytes.len()))
            }
            Self::SemanticVersions(records) => records.iter().fold(0_usize, |bound, record| {
                bound
                    .saturating_add(fixed_record_bound())
                    .saturating_add(package_reference_bound(&record.package))
                    .saturating_add(record.coordinate.as_str().len())
            }),
            Self::SemanticVersionSelected(record) => fixed_record_bound()
                .saturating_add(package_reference_bound(&record.package))
                .saturating_add(record.coordinate.as_str().len()),
            Self::Dependents(RegistryMetadata::NotRecorded(reason))
            | Self::Owner(RegistryMetadata::NotRecorded(reason)) => text_bound(reason),
            Self::Dependencies(crate::DependencyFacts::Known(records)) => records
                .iter()
                .fold(0_usize, |bound, record| {
                    bound.saturating_add(dependency_record_bound(record))
                }),
            Self::Dependencies(crate::DependencyFacts::Unknown(reason))
            | Self::Dependencies(crate::DependencyFacts::Unavailable(reason)) => text_bound(reason),
            Self::PackageProfile { latest, .. } => {
                latest.as_ref().map_or(64, registry_package_record_bound)
            }
            Self::Subscribed(record) => subscription_record_bound(record),
            Self::Unsubscribed(_) | Self::ProjectDeleted(_) | Self::TreeClosed(_) => 64,
            Self::Subscriptions(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(subscription_record_bound(record))
            }),
            Self::Releases(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(release_record_bound(record))
            }),
            Self::Projects(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(project_record_bound(record))
            }),
            Self::ProjectCreated(record)
            | Self::ProjectAdded(record)
            | Self::ProjectRemoved(record)
            | Self::ProjectSynced(record) => project_record_bound(record),
            Self::Tree(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(tree_node_record_bound(record))
            }),
            Self::TreeOpened(record) => tree_node_record_bound(record),
        };
        ENVELOPE_BYTES.saturating_add(payload)
    }
}

const fn fixed_record_bound() -> usize {
    512
}

fn text_bound(text: &ProductText) -> usize {
    text.as_str().len().saturating_add(32)
}

fn optional_text_bound(text: Option<&ProductText>) -> usize {
    text.map_or(16, text_bound)
}

fn package_reference_bound(package: &PackageReference) -> usize {
    package.as_str().len().saturating_add(64)
}

fn declaration_record_bound(record: &DeclarationRecord) -> usize {
    fixed_record_bound()
        .saturating_add(text_bound(&record.label))
        .saturating_add(optional_text_bound(record.signature.as_ref()))
}

fn diff_record_bound(record: &DiffRecord) -> usize {
    record.links.iter().fold(
        fixed_record_bound().saturating_add(text_bound(&record.label)),
        |bound, link| bound.saturating_add(semantic_link_delta_bound(link)),
    )
}

fn semantic_link_delta_bound(delta: &SemanticLinkDelta) -> usize {
    let evidence = match delta {
        SemanticLinkDelta::Added { evidence, .. } | SemanticLinkDelta::Removed { evidence, .. } => {
            semantic_link_evidence_bound(evidence)
        }
        SemanticLinkDelta::EvidenceChanged { before, after, .. } => {
            semantic_link_evidence_bound(before).saturating_add(semantic_link_evidence_bound(after))
        }
    };
    fixed_record_bound().saturating_add(evidence)
}

fn semantic_link_evidence_bound(evidence: &SemanticLinkEvidence) -> usize {
    evidence.source.as_ref().map_or(128, |source| {
        fixed_record_bound().saturating_add(text_bound(&source.file))
    })
}

fn registry_records_bound(records: &[RegistryPackageRecord]) -> usize {
    records.iter().fold(0_usize, |bound, record| {
        bound.saturating_add(registry_package_record_bound(record))
    })
}

fn registry_package_record_bound(record: &RegistryPackageRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.coordinate))
        .saturating_add(text_bound(&record.name))
        .saturating_add(text_bound(&record.version))
        .saturating_add(
            serde_json::to_vec(&record.native_metadata).map_or(0, |bytes| bytes.len()),
        )
        .saturating_add(serde_json::to_vec(&record.advisory).map_or(0, |bytes| bytes.len()))
}

fn valid_registry_record(record: &RegistryPackageRecord) -> bool {
    record.native_metadata.admit().is_ok()
        && record
            .native_metadata
            .identity()
            .is_ok_and(|identity| identity == record.native_metadata_version)
}

fn dependency_record_bound(record: &crate::PackageDependencyRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.source))
        .saturating_add(text_bound(&record.target.name))
        .saturating_add(text_bound(&record.target.requirement))
        .saturating_add(
            record
                .target
                .resolved
                .as_ref()
                .map_or(0, package_reference_bound),
        )
}

fn subscription_record_bound(record: &SubscriptionRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.package))
        .saturating_add(optional_text_bound(record.seen.as_ref()))
}

fn release_record_bound(record: &ReleaseRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.package))
        .saturating_add(text_bound(&record.version))
}

fn project_record_bound(record: &ProjectRecord) -> usize {
    record.members.iter().fold(
        fixed_record_bound()
            .saturating_add(record.name.as_str().len())
            .saturating_add(optional_text_bound(record.lockfile.as_ref())),
        |bound, package| bound.saturating_add(package_reference_bound(package)),
    )
}

fn tree_node_record_bound(record: &TreeNodeRecord) -> usize {
    fixed_record_bound()
        .saturating_add(tree_subject_bound(&record.subject))
        .saturating_add(text_bound(&record.title))
        .saturating_add(match &record.opener {
            TreeOpener::Mcp(client) => text_bound(client),
            TreeOpener::Desktop | TreeOpener::Cli => 32,
        })
}

fn tree_subject_bound(subject: &TreeSubject) -> usize {
    match subject {
        TreeSubject::Package(package) => package_reference_bound(package),
        TreeSubject::Declaration(text) | TreeSubject::Search(text) | TreeSubject::Owner(text) => {
            text_bound(text)
        }
        TreeSubject::Explore(query) => optional_text_bound(query.as_ref()),
    }
}

/// Semantic refusal while admitting a parsed product value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductAdmissionError {
    /// Required text was blank.
    EmptyText,
    /// Text exceeded its byte bound or contained NUL.
    TextBound,
    /// Package locator grammar was invalid.
    PackageReference,
    /// Project name grammar was invalid.
    ProjectName,
    /// A collection exceeded its row bound.
    RowBound,
    /// Reply and command identities differ.
    CommandMismatch,
    /// A semantic diff contradicts its declaration sides or graph evidence lifecycle.
    DiffShape,
    /// A semantic generation record or selection is internally inconsistent.
    SemanticVersionShape,
    /// A dependency row has a stale or duplicated content identity.
    DependencyShape,
    /// Native registry metadata is malformed, oversized, or has a stale identity.
    NativeMetadata,
}
impl core::fmt::Display for ProductAdmissionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::EmptyText => "product text is empty",
            Self::TextBound => "product text exceeds its bound or contains NUL",
            Self::PackageReference => "package reference is not canonical",
            Self::ProjectName => "project name is invalid",
            Self::RowBound => "product row count exceeds its bound",
            Self::CommandMismatch => "product reply does not match its command",
            Self::DiffShape => "semantic diff has inconsistent identities or evidence",
            Self::SemanticVersionShape => "semantic version selection is inconsistent",
            Self::DependencyShape => "dependency fact has an invalid or duplicate identity",
            Self::NativeMetadata => "native registry metadata is invalid or has a stale identity",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_command_and_reply_round_trip_with_safe_unknown_state() {
        let command = SurfaceCommand::Advisory {
            package: PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package"),
            override_evidence: Some(OverrideEvidence {
                actor: "release-bot".to_owned(),
                reason: "reviewed emergency pin".to_owned(),
                policy_version: 3,
                expires_at: Some(4_102_444_800),
            }),
        };
        command.admit().expect("advisory command admission");
        let command_json = serde_json::to_vec(&command).expect("command encoding");
        let decoded: SurfaceCommand = serde_json::from_slice(&command_json).expect("command decoding");
        assert_eq!(decoded, command);

        let reply = SurfaceReply::Advisory(AdvisoryPackageDto::unknown());
        reply.admit(CommandId::Advisory).expect("advisory reply admission");
        let reply_json = serde_json::to_vec(&reply).expect("reply encoding");
        let decoded: SurfaceReply = serde_json::from_slice(&reply_json).expect("reply decoding");
        assert_eq!(decoded, reply);
        let SurfaceReply::Advisory(dto) = decoded else {
            panic!("advisory reply shape");
        };
        assert!(matches!(
            dto.decision,
            backend_advisory::AcquisitionDecision::Deny(_)
        ));
    }

    #[test]
    fn registry_download_coverage_round_trips_as_a_typed_state() {
        let native_metadata =
            RegistryNativeMetadata::unavailable(RegistryEcosystem::Cargo, "test fixture");
        let record = RegistryPackageRecord {
            coordinate: PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package"),
            ecosystem: RegistryEcosystem::Cargo,
            name: ProductText::new("demo").expect("name"),
            version: ProductText::new("1.0.0").expect("version"),
            bytes: 12,
            standing: RegistryReleaseStanding::Available,
            downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
            facts_version: [0; 32],
            native_metadata_version: native_metadata.identity().expect("native metadata identity"),
            native_metadata,
            advisory: AdvisoryPackageDto::unknown(),
        };
        let reply = SurfaceReply::Explored(vec![record].into_boxed_slice());
        let encoded = serde_json::to_vec(&reply).expect("registry reply encoding");
        let decoded: SurfaceReply =
            serde_json::from_slice(&encoded).expect("registry reply decoding");
        assert_eq!(decoded, reply);
        let SurfaceReply::Explored(records) = decoded else {
            panic!("registry reply shape");
        };
        assert_eq!(
            records[0].downloads,
            RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported)
        );
    }
}
