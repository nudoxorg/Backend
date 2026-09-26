//! Lazy async Trustfall execution over immutable typed semantic evidence.

use backend_library::PackageKey;
use backend_version::WorkspaceRoot;
use backend_semantic::ir::{
    DeclarationIdentity, ExternalTargetIdentity, ImageProvenance, SemanticImageAuthority,
    SemanticImageFacts,
};
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl};
use futures_core::Stream;
use futures_util::stream;
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use thiserror::Error;
use trustfall::provider::async_helpers::{
    resolve_coercion_with, resolve_neighbors_with, resolve_property_with,
};
use trustfall::provider::{
    AsVertex, AsyncBasicAdapter, AsyncContextOutcomeStream, AsyncContextStream,
    AsyncNeighborStream, EdgeParameters, Typename,
};
use trustfall::{FieldValue, QueryResultStream, Schema};

const SCHEMA: &str = r"
schema { query: Root }

type Root {
  Item: [CodeItem!]!
  Project: [CodeItem!]!
  Declaration: [CodeItem!]!
  ExternalTarget: [CodeItem!]!
}

type CodeItem {
  id: String!
  kind: String!
  coordinate: String!
  name: String!
  signature: String
  documentation: String!
  score: Int
  state: String!
  revision: String!
  profile: String
  packageUrl: String
  project: [CodeItem!]!
  parent: [CodeItem!]!
  children: [CodeItem!]!
  related: [CodeItem!]!
  referencedBy: [CodeItem!]!
  sameProject: [CodeItem!]!
}
";

static PARSED_SCHEMA: OnceLock<Result<Schema, String>> = OnceLock::new();

/// Failure to admit, parse, or execute a query over typed semantic evidence.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum QueryError {
    /// A row, field, request, page, or aggregate limit was exceeded.
    #[error("Trustfall semantic query exceeded its bounded execution envelope")]
    InvalidLimit,
    /// Typed evidence or one of its authority relationships was invalid.
    #[error("Trustfall semantic evidence was rejected: {0}")]
    Evidence(String),
    /// Cancellation was observed before or during execution.
    #[error("Trustfall semantic query was cancelled")]
    Cancelled,
    /// The static Trustfall schema could not be parsed.
    #[error("Trustfall semantic schema was rejected: {0}")]
    Schema(String),
    /// Trustfall rejected the supplied query document or variables.
    #[error("Trustfall semantic query was rejected: {0}")]
    Query(String),
}

/// Exact product package evidence used only as a navigation root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageScopeEvidence {
    package: PackageKey,
}

impl PackageScopeEvidence {
    /// Retains one already-admitted typed package key.
    #[must_use]
    pub const fn new(package: PackageKey) -> Self {
        Self { package }
    }

    /// Returns the typed package key.
    #[must_use]
    pub const fn package(self) -> PackageKey {
        self.package
    }

    /// Returns the canonical product row identity for this package.
    #[must_use]
    pub fn row_id(self) -> String {
        backend_library::RowId::Package(self.package).stable_key()
    }
}

/// Compiler-owned semantic authority retained for one declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerSemanticEvidence {
    package: PackageKey,
    coordinate: PackageUrl,
    profile: LanguageProfile,
    declaration: DeclarationIdentity,
    image: [u8; 32],
    facts: SemanticImageFacts,
}

impl CompilerSemanticEvidence {
    /// Retains one declaration together with its exact compiler image authority.
    #[must_use]
    pub fn new(
        package: PackageKey,
        coordinate: PackageUrl,
        profile: LanguageProfile,
        declaration: DeclarationIdentity,
        image: [u8; 32],
        facts: SemanticImageFacts,
    ) -> Self {
        Self {
            package,
            coordinate,
            profile,
            declaration,
            image,
            facts,
        }
    }

    /// Returns the package that owns the declaration.
    #[must_use]
    pub const fn package(&self) -> PackageKey {
        self.package
    }

    /// Returns the exact source language profile.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    /// Returns the complete canonical package URL.
    #[must_use]
    pub const fn coordinate(&self) -> &PackageUrl {
        &self.coordinate
    }

    /// Returns the compiler-issued declaration identity.
    #[must_use]
    pub const fn declaration(&self) -> DeclarationIdentity {
        self.declaration
    }

    /// Returns the immutable image commitment.
    #[must_use]
    pub const fn image(&self) -> [u8; 32] {
        self.image
    }

    /// Returns the image-level authority facts.
    #[must_use]
    pub const fn facts(&self) -> SemanticImageFacts {
        self.facts
    }

    /// Returns the canonical package-and-declaration product row identity.
    #[must_use]
    pub fn row_id(&self) -> String {
        compiler_row_id(self.package, self.declaration)
    }
}

/// Compiler-owned evidence for one unresolved or cross-fragment endpoint.
///
/// The endpoint retains its own typed identity and the exact immutable image
/// that supplied it. It is deliberately distinct from declaration evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerExternalTargetEvidence {
    package: PackageKey,
    coordinate: PackageUrl,
    profile: LanguageProfile,
    target: ExternalTargetIdentity,
    image: [u8; 32],
    facts: SemanticImageFacts,
}

impl CompilerExternalTargetEvidence {
    /// Retains the exact package, profile, endpoint, and image authority.
    #[must_use]
    pub fn new(
        package: PackageKey,
        coordinate: PackageUrl,
        profile: LanguageProfile,
        target: ExternalTargetIdentity,
        image: [u8; 32],
        facts: SemanticImageFacts,
    ) -> Self {
        Self {
            package,
            coordinate,
            profile,
            target,
            image,
            facts,
        }
    }

    /// Returns the package that owns this external reference.
    #[must_use]
    pub const fn package(&self) -> PackageKey {
        self.package
    }

    /// Returns the exact source language profile.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    /// Returns the exact package coordinate admitted for this image.
    #[must_use]
    pub const fn coordinate(&self) -> &PackageUrl {
        &self.coordinate
    }

    /// Returns the coordinate-free external endpoint identity.
    #[must_use]
    pub const fn target(&self) -> ExternalTargetIdentity {
        self.target
    }

    /// Returns the immutable image commitment that supplied the endpoint.
    #[must_use]
    pub const fn image(&self) -> [u8; 32] {
        self.image
    }

    /// Returns the compiler authority facts carried by the image.
    #[must_use]
    pub const fn facts(&self) -> SemanticImageFacts {
        self.facts
    }

    /// Returns the only product row identity admitted for this scoped target.
    #[must_use]
    pub fn row_id(&self) -> String {
        external_row_id(self.package, self.image, self.target)
    }
}

/// Structural evidence retained only when no complete compiler profile exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StructuralFallbackEvidence {
    package: PackageKey,
    profile: LanguageProfile,
    content: [u8; 32],
    producer: [u8; 32],
}

impl StructuralFallbackEvidence {
    /// Retains an explicitly structural result and its producing content facts.
    #[must_use]
    pub const fn new(
        package: PackageKey,
        profile: LanguageProfile,
        content: [u8; 32],
        producer: [u8; 32],
    ) -> Self {
        Self {
            package,
            profile,
            content,
            producer,
        }
    }

    /// Returns the package that owns the structural result.
    #[must_use]
    pub const fn package(self) -> PackageKey {
        self.package
    }

    /// Returns the profile selected by the structural grammar.
    #[must_use]
    pub const fn profile(self) -> LanguageProfile {
        self.profile
    }
}

/// Closed authority alternatives accepted by the graph extension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticQueryEvidence {
    /// Typed package navigation scope.
    Package(PackageScopeEvidence),
    /// Compiler-owned declaration facts.
    Compiler(CompilerSemanticEvidence),
    /// Compiler-owned external target facts, distinct from declarations.
    CompilerExternalTarget(CompilerExternalTargetEvidence),
    /// Explicitly non-semantic structural fallback facts.
    StructuralFallback(StructuralFallbackEvidence),
}

impl SemanticQueryEvidence {
    fn package(&self) -> PackageKey {
        match self {
            Self::Package(value) => value.package(),
            Self::Compiler(value) => value.package(),
            Self::CompilerExternalTarget(value) => value.package(),
            Self::StructuralFallback(value) => value.package(),
        }
    }

    fn state(&self) -> &'static str {
        match self {
            Self::Package(_) => "package_scope",
            Self::Compiler(_) => "compiler_semantic",
            Self::CompilerExternalTarget(_) => "compiler_external_target",
            Self::StructuralFallback(_) => "structural_fallback",
        }
    }

    fn profile(&self) -> Option<LanguageProfile> {
        match self {
            Self::Package(_) => None,
            Self::Compiler(value) => Some(value.profile()),
            Self::CompilerExternalTarget(value) => Some(value.profile()),
            Self::StructuralFallback(value) => Some(value.profile()),
        }
    }

    fn package_url(&self) -> Option<&str> {
        match self {
            Self::Compiler(value) => Some(value.coordinate().as_str()),
            Self::CompilerExternalTarget(value) => Some(value.coordinate().as_str()),
            Self::Package(_) | Self::StructuralFallback(_) => None,
        }
    }
}

fn external_row_id(package: PackageKey, image: [u8; 32], target: ExternalTargetIdentity) -> String {
    let scoped = target.in_scope(package.to_bytes(), image);
    let preimage = backend_library::encode_id(scoped.as_bytes());
    backend_library::RowId::Symbol(backend_library::symbol_key(&preimage)).stable_key()
}

fn compiler_row_id(package: PackageKey, declaration: DeclarationIdentity) -> String {
    let mut preimage = backend_library::encode_id(package.as_bytes());
    preimage.push_str("::");
    preimage.push_str(&encode_hex(declaration.family.as_bytes()));
    preimage.push_str(&encode_hex(declaration.variant.as_bytes()));
    backend_library::RowId::Symbol(backend_library::symbol_key(&preimage)).stable_key()
}

/// Human-facing fields and typed graph coordinates for one evidence row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticQueryPresentation {
    /// Stable typed row identity rendered for Trustfall.
    pub id: String,
    /// Closed or producer-derived item kind presentation.
    pub kind: String,
    /// Human-facing coordinate; never used as identity authority.
    pub coordinate: String,
    /// Human-facing item name.
    pub name: String,
    /// Canonical compiler signature when captured.
    pub signature: Option<String>,
    /// Rendered semantic documentation.
    pub documentation: String,
    /// Optional search score supplied by an admitted upstream result.
    pub score: Option<u32>,
    /// Stable row identity of the owning package.
    pub project: Option<String>,
    /// Stable row identity of the semantic parent.
    pub parent: Option<String>,
    /// Stable row identities of typed graph neighbors.
    pub related: Box<[String]>,
}

/// One typed evidence row admitted into an immutable query corpus.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticQueryFact {
    evidence: SemanticQueryEvidence,
    presentation: SemanticQueryPresentation,
}

impl SemanticQueryFact {
    /// Couples typed authority evidence to its bounded presentation.
    #[must_use]
    pub fn new(evidence: SemanticQueryEvidence, presentation: SemanticQueryPresentation) -> Self {
        Self {
            evidence,
            presentation,
        }
    }

    /// Returns the typed authority evidence.
    #[must_use]
    pub const fn evidence(&self) -> &SemanticQueryEvidence {
        &self.evidence
    }

    /// Returns the human-facing fields and graph coordinates.
    #[must_use]
    pub const fn presentation(&self) -> &SemanticQueryPresentation {
        &self.presentation
    }
}

/// Immutable typed semantic corpus bound to one checked workspace publication.
#[derive(Clone, Debug)]
pub struct SemanticQueryCorpus {
    workspace: WorkspaceRoot,
    evidence_digest: [u8; 32],
    facts: Arc<[SemanticQueryFact]>,
    limits: crate::Limits,
}

impl SemanticQueryCorpus {
    /// Validates typed authority, stable identities, and every graph edge.
    pub fn admit(
        workspace: WorkspaceRoot,
        facts: Vec<SemanticQueryFact>,
    ) -> Result<Self, QueryError> {
        Self::admit_with_limits(workspace, facts, crate::Limits::default())
    }

    /// Validates a corpus against one explicit envelope retained by every
    /// later request over these facts.
    pub fn admit_with_limits(
        workspace: WorkspaceRoot,
        facts: Vec<SemanticQueryFact>,
        limits: crate::Limits,
    ) -> Result<Self, QueryError> {
        let limits = limits.validate().map_err(|_| QueryError::InvalidLimit)?;
        if facts.len() > limits.max_rows {
            return Err(QueryError::InvalidLimit);
        }
        let mut ids = BTreeMap::<&str, usize>::new();
        let mut admitted_bytes = 0_usize;
        for (index, fact) in facts.iter().enumerate() {
            admit_fact_envelope(fact, limits, &mut admitted_bytes)?;
            let row = fact.presentation();
            if row.id.is_empty()
                || row.kind.is_empty()
                || row.coordinate.is_empty()
                || row.name.is_empty()
            {
                return Err(QueryError::Evidence(
                    "semantic presentation contains an empty required field".to_owned(),
                ));
            }
            if ids.insert(&row.id, index).is_some() {
                return Err(QueryError::Evidence(
                    "semantic corpus contains a duplicate row identity".to_owned(),
                ));
            }
            match fact.evidence() {
                SemanticQueryEvidence::Package(value) => {
                    if row.project.is_some() || row.id != value.row_id() {
                        return Err(QueryError::Evidence(
                            "package presentation differs from its typed identity".to_owned(),
                        ));
                    }
                }
                SemanticQueryEvidence::Compiler(value) => {
                    admit_compiler_image(value.coordinate(), value.profile(), value.facts())?;
                    if row.id != value.row_id() {
                        return Err(QueryError::Evidence(
                            "compiler declaration presentation differs from its typed identity"
                                .to_owned(),
                        ));
                    }
                }
                SemanticQueryEvidence::CompilerExternalTarget(value) => {
                    admit_compiler_image(value.coordinate(), value.profile(), value.facts())?;
                    if row.kind != "external" || row.id != value.row_id() {
                        return Err(QueryError::Evidence(
                            "compiler external target presentation differs from its typed identity"
                                .to_owned(),
                        ));
                    }
                    if row.parent.is_some() || !row.related.is_empty() {
                        return Err(QueryError::Evidence(
                            "compiler external target cannot claim declaration graph ownership"
                                .to_owned(),
                        ));
                    }
                }
                SemanticQueryEvidence::StructuralFallback(_) => {}
            }
        }
        for fact in &facts {
            let row = fact.presentation();
            if let Some(project) = row.project.as_deref() {
                let Some(project) = ids.get(project).map(|index| &facts[*index]) else {
                    return Err(QueryError::Evidence(
                        "semantic row refers to a missing project".to_owned(),
                    ));
                };
                if !matches!(project.evidence(), SemanticQueryEvidence::Package(_))
                    || project.evidence().package() != fact.evidence().package()
                {
                    return Err(QueryError::Evidence(
                        "semantic row crossed its typed package scope".to_owned(),
                    ));
                }
            } else if !matches!(fact.evidence(), SemanticQueryEvidence::Package(_)) {
                return Err(QueryError::Evidence(
                    "declaration evidence has no typed package scope".to_owned(),
                ));
            }
            for target in row.parent.iter().chain(row.related.iter()) {
                let Some(target) = ids.get(target.as_str()).map(|index| &facts[*index]) else {
                    return Err(QueryError::Evidence(
                        "semantic row refers to a missing graph target".to_owned(),
                    ));
                };
                if target.evidence().package() != fact.evidence().package() {
                    return Err(QueryError::Evidence(
                        "semantic graph edge crossed its package authority".to_owned(),
                    ));
                }
                if let SemanticQueryEvidence::CompilerExternalTarget(external) = target.evidence() {
                    let SemanticQueryEvidence::Compiler(source) = fact.evidence() else {
                        return Err(QueryError::Evidence(
                            "compiler external target has no compiler declaration source"
                                .to_owned(),
                        ));
                    };
                    if source.image() != external.image()
                        || source.profile() != external.profile()
                        || source.coordinate() != external.coordinate()
                        || source.facts() != external.facts()
                    {
                        return Err(QueryError::Evidence(
                            "compiler external edge crossed its exact image authority".to_owned(),
                        ));
                    }
                }
            }
        }
        let evidence_digest = digest_corpus(workspace, &facts);
        Ok(Self {
            workspace,
            evidence_digest,
            facts: facts.into(),
            limits,
        })
    }

    /// Returns the workspace publication that owns this corpus.
    #[must_use]
    pub const fn workspace(&self) -> WorkspaceRoot {
        self.workspace
    }

    /// Returns the canonical commitment to all admitted evidence and edges.
    #[must_use]
    pub const fn evidence_digest(&self) -> [u8; 32] {
        self.evidence_digest
    }

    /// Returns all immutable facts in their admitted canonical order.
    #[must_use]
    pub fn facts(&self) -> &[SemanticQueryFact] {
        &self.facts
    }

    /// Returns the exact envelope admitted with this corpus.
    #[must_use]
    pub const fn limits(&self) -> crate::Limits {
        self.limits
    }
}

fn admit_fact_envelope(
    fact: &SemanticQueryFact,
    limits: crate::Limits,
    admitted_bytes: &mut usize,
) -> Result<(), QueryError> {
    let row = fact.presentation();
    let field_count = 5_usize
        .checked_add(usize::from(row.signature.is_some()))
        .and_then(|count| count.checked_add(usize::from(row.score.is_some())))
        .and_then(|count| count.checked_add(usize::from(row.project.is_some())))
        .and_then(|count| count.checked_add(usize::from(row.parent.is_some())))
        .and_then(|count| count.checked_add(row.related.len()))
        .ok_or(QueryError::InvalidLimit)?;
    if field_count > limits.max_fields_per_row {
        return Err(QueryError::InvalidLimit);
    }
    for value in [
        row.id.as_str(),
        row.kind.as_str(),
        row.coordinate.as_str(),
        row.name.as_str(),
        row.documentation.as_str(),
    ] {
        admit_text(value, limits, admitted_bytes)?;
    }
    for value in row
        .signature
        .iter()
        .chain(row.project.iter())
        .chain(row.parent.iter())
        .chain(row.related.iter())
    {
        admit_text(value, limits, admitted_bytes)?;
    }
    if row.score.is_some() {
        charge_bytes(4, limits, admitted_bytes)?;
    }
    match fact.evidence() {
        SemanticQueryEvidence::Package(_) => charge_bytes(32, limits, admitted_bytes),
        SemanticQueryEvidence::Compiler(value) => {
            admit_text(value.coordinate().as_str(), limits, admitted_bytes)?;
            charge_bytes(252, limits, admitted_bytes)
        }
        SemanticQueryEvidence::CompilerExternalTarget(value) => {
            admit_text(value.coordinate().as_str(), limits, admitted_bytes)?;
            charge_bytes(252, limits, admitted_bytes)
        }
        SemanticQueryEvidence::StructuralFallback(_) => charge_bytes(98, limits, admitted_bytes),
    }
}

fn admit_text(
    value: &str,
    limits: crate::Limits,
    admitted_bytes: &mut usize,
) -> Result<(), QueryError> {
    if value.len() > limits.max_field_bytes {
        return Err(QueryError::InvalidLimit);
    }
    charge_bytes(value.len(), limits, admitted_bytes)
}

fn charge_bytes(
    bytes: usize,
    limits: crate::Limits,
    admitted_bytes: &mut usize,
) -> Result<(), QueryError> {
    *admitted_bytes = admitted_bytes
        .checked_add(bytes)
        .filter(|total| *total <= limits.max_total_bytes)
        .ok_or(QueryError::InvalidLimit)?;
    Ok(())
}

fn admit_compiler_image(
    coordinate: &PackageUrl,
    profile: LanguageProfile,
    facts: SemanticImageFacts,
) -> Result<(), QueryError> {
    if coordinate.package_type().language() != profile.language()
        || facts.authority != SemanticImageAuthority::Language(profile)
    {
        return Err(QueryError::Evidence(
            "compiler evidence language authority differs from its package coordinate".to_owned(),
        ));
    }
    let ImageProvenance::Captured { recipe, .. } = facts.provenance else {
        return Err(QueryError::Evidence(
            "compiler evidence lacks captured image provenance".to_owned(),
        ));
    };
    if recipe.profile != profile {
        return Err(QueryError::Evidence(
            "compiler evidence recipe differs from its language profile".to_owned(),
        ));
    }
    Ok(())
}

fn digest_corpus(workspace: WorkspaceRoot, facts: &[SemanticQueryFact]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.trustfall.semantic-corpus.v2\0");
    hasher.update(workspace.as_bytes());
    hash_count(&mut hasher, facts.len());
    for fact in facts {
        let row = fact.presentation();
        hash_bytes(&mut hasher, 0, row.id.as_bytes());
        hash_bytes(&mut hasher, 1, row.kind.as_bytes());
        hash_bytes(&mut hasher, 2, row.coordinate.as_bytes());
        hash_bytes(&mut hasher, 3, row.name.as_bytes());
        hash_optional_bytes(&mut hasher, 4, row.signature.as_deref().map(str::as_bytes));
        hash_bytes(&mut hasher, 5, row.documentation.as_bytes());
        hash_optional_u32(&mut hasher, 6, row.score);
        hash_optional_bytes(&mut hasher, 7, row.project.as_deref().map(str::as_bytes));
        hash_optional_bytes(&mut hasher, 8, row.parent.as_deref().map(str::as_bytes));
        hasher.update(&[9]);
        hash_count(&mut hasher, row.related.len());
        for target in &row.related {
            hash_unlabeled_bytes(&mut hasher, target.as_bytes());
        }
        match fact.evidence() {
            SemanticQueryEvidence::Package(value) => {
                hasher.update(&[0]);
                hasher.update(value.package().as_bytes());
            }
            SemanticQueryEvidence::Compiler(value) => {
                hasher.update(&[1]);
                hasher.update(value.package().as_bytes());
                hash_unlabeled_bytes(&mut hasher, value.coordinate().as_str().as_bytes());
                hasher.update(&<[u8; 2]>::from(value.profile()));
                hasher.update(value.declaration().family.as_bytes());
                hasher.update(value.declaration().variant.as_bytes());
                hasher.update(&value.image());
                hash_image_facts(&mut hasher, value.facts());
            }
            SemanticQueryEvidence::CompilerExternalTarget(value) => {
                hasher.update(&[3]);
                hasher.update(value.package().as_bytes());
                hash_unlabeled_bytes(&mut hasher, value.coordinate().as_str().as_bytes());
                hasher.update(&<[u8; 2]>::from(value.profile()));
                hasher.update(value.target().as_bytes());
                hasher.update(&value.image());
                hash_image_facts(&mut hasher, value.facts());
            }
            SemanticQueryEvidence::StructuralFallback(value) => {
                hasher.update(&[2]);
                hasher.update(value.package.as_bytes());
                hasher.update(&<[u8; 2]>::from(value.profile));
                hasher.update(&value.content);
                hasher.update(&value.producer);
            }
        }
    }
    *hasher.finalize().as_bytes()
}

fn hash_bytes(hasher: &mut blake3::Hasher, tag: u8, bytes: &[u8]) {
    hasher.update(&[tag]);
    hash_unlabeled_bytes(hasher, bytes);
}

fn hash_unlabeled_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn hash_count(hasher: &mut blake3::Hasher, count: usize) {
    hasher.update(&(count as u64).to_be_bytes());
}

fn hash_optional_bytes(hasher: &mut blake3::Hasher, tag: u8, value: Option<&[u8]>) {
    hasher.update(&[tag]);
    match value {
        Some(bytes) => {
            hasher.update(&[1]);
            hash_unlabeled_bytes(hasher, bytes);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_u32(hasher: &mut blake3::Hasher, tag: u8, value: Option<u32>) {
    hasher.update(&[tag]);
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_image_facts(hasher: &mut blake3::Hasher, facts: SemanticImageFacts) {
    match facts.authority {
        SemanticImageAuthority::Shared => {
            hasher.update(&[0]);
        }
        SemanticImageAuthority::Language(profile) => {
            hasher.update(&[1]);
            hasher.update(&<[u8; 2]>::from(profile));
        }
    };
    match facts.provenance {
        ImageProvenance::Unavailable => {
            hasher.update(&[0]);
        }
        ImageProvenance::Captured {
            source,
            recipe,
            claim,
            scope,
        } => {
            hasher.update(&[1]);
            hasher.update(source.identity.as_ref());
            hasher.update(&source.byte_len.to_be_bytes());
            hasher.update(recipe.identity.as_ref());
            hasher.update(&<[u8; 2]>::from(recipe.profile));
            hasher.update(&[u8::from(recipe.stage), u8::from(recipe.tool)]);
            hasher.update(recipe.toolchain.as_ref());
            hasher.update(claim.identity.as_ref());
            for atom in [scope.ecosystem, scope.package, scope.path] {
                hasher.update(&atom.raw.to_be_bytes());
            }
            hash_optional_u32(&mut *hasher, 0, scope.coordinate.map(|atom| atom.raw));
        }
    }
}

/// Read-only cancellation observation bound into one admitted query.
#[derive(Clone, Debug)]
pub struct SemanticQueryCancellation {
    cancelled: Arc<AtomicBool>,
}

/// Exclusive control used to cancel queries sharing one observation.
#[derive(Clone, Debug)]
pub struct SemanticQueryCancelHandle {
    cancelled: Arc<AtomicBool>,
}

impl SemanticQueryCancellation {
    /// Creates paired read-only cancellation evidence and exclusive control.
    #[must_use]
    pub fn new() -> (Self, SemanticQueryCancelHandle) {
        let cancelled = Arc::new(AtomicBool::new(false));
        (
            Self {
                cancelled: Arc::clone(&cancelled),
            },
            SemanticQueryCancelHandle { cancelled },
        )
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl SemanticQueryCancelHandle {
    /// Publishes cancellation to every query sharing this handle.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Exact immutable inputs shared by every row and terminal of one query.
#[derive(Clone, Debug)]
pub struct SemanticQueryIdentity {
    workspace: WorkspaceRoot,
    evidence_digest: [u8; 32],
    query: Arc<str>,
    variables: Arc<BTreeMap<String, FieldValue>>,
    limit: usize,
    limits: crate::Limits,
}

impl SemanticQueryIdentity {
    /// Returns the exact workspace root.
    #[must_use]
    pub const fn workspace(&self) -> WorkspaceRoot {
        self.workspace
    }

    /// Returns the exact corpus evidence commitment.
    #[must_use]
    pub const fn evidence_digest(&self) -> [u8; 32] {
        self.evidence_digest
    }

    /// Returns the admitted Trustfall query text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns the admitted immutable variable map.
    #[must_use]
    pub fn variables(&self) -> &BTreeMap<String, FieldValue> {
        &self.variables
    }

    /// Returns the requested page row limit.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the exact corpus and request envelope.
    #[must_use]
    pub const fn limits(&self) -> crate::Limits {
        self.limits
    }

    /// Reports whether another event belongs to this exact admitted request.
    ///
    /// Paging offsets live in the transport continuation; every event within
    /// one admitted page carries the immutable corpus, query, variables, and
    /// execution envelope compared here.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.workspace == other.workspace
            && self.evidence_digest == other.evidence_digest
            && self.query == other.query
            && self.variables == other.variables
            && self.limit == other.limit
            && self.limits == other.limits
    }
}

/// One fully admitted typed semantic query.
pub struct SemanticQueryRequest {
    corpus: SemanticQueryCorpus,
    identity: Arc<SemanticQueryIdentity>,
    cancellation: SemanticQueryCancellation,
    start_offset: usize,
}

impl SemanticQueryRequest {
    /// Returns the exact identity that every event from this request must carry.
    #[must_use]
    pub fn identity(&self) -> &SemanticQueryIdentity {
        &self.identity
    }

    /// Admits one bounded page request against the corpus's retained envelope.
    pub fn admit_page(
        corpus: SemanticQueryCorpus,
        query: impl Into<Arc<str>>,
        variables: BTreeMap<String, FieldValue>,
        start_offset: usize,
        limit: usize,
        cancellation: SemanticQueryCancellation,
    ) -> Result<Self, QueryError> {
        let limits = corpus.limits();
        if limit == 0
            || limit > limits.max_page
            || start_offset
                .checked_add(limit)
                .is_none_or(|end| end > limits.max_rows)
            || corpus.facts().len() > limits.max_rows
        {
            return Err(QueryError::InvalidLimit);
        }
        if cancellation.is_cancelled() {
            return Err(QueryError::Cancelled);
        }
        let query = query.into();
        admit_query_input(&query, &variables, limits)?;
        let identity = Arc::new(SemanticQueryIdentity {
            workspace: corpus.workspace(),
            evidence_digest: corpus.evidence_digest(),
            query,
            variables: Arc::new(variables),
            limit,
            limits,
        });
        Ok(Self {
            corpus,
            identity,
            cancellation,
            start_offset,
        })
    }
}

const MAX_VARIABLE_DEPTH: usize = 8;

fn admit_query_input(
    query: &str,
    variables: &BTreeMap<String, FieldValue>,
    limits: crate::Limits,
) -> Result<(), QueryError> {
    if query.trim().is_empty()
        || query.len() > limits.max_field_bytes
        || variables.len() > limits.max_fields_per_row
    {
        return Err(QueryError::InvalidLimit);
    }
    let mut bytes = query.len();
    let mut values = 0_usize;
    for (name, value) in variables {
        if name.is_empty() || name.len() > limits.max_field_bytes {
            return Err(QueryError::InvalidLimit);
        }
        bytes = bytes
            .checked_add(name.len())
            .filter(|total| *total <= limits.max_total_bytes)
            .ok_or(QueryError::InvalidLimit)?;
        admit_field_value(value, 0, limits, &mut values, &mut bytes)?;
    }
    Ok(())
}

fn admit_field_value(
    value: &FieldValue,
    depth: usize,
    limits: crate::Limits,
    values: &mut usize,
    bytes: &mut usize,
) -> Result<(), QueryError> {
    if depth > MAX_VARIABLE_DEPTH {
        return Err(QueryError::InvalidLimit);
    }
    *values = values
        .checked_add(1)
        .filter(|count| *count <= limits.max_fields_per_row)
        .ok_or(QueryError::InvalidLimit)?;
    let own_bytes = match value {
        FieldValue::Null => 0,
        FieldValue::Boolean(_) => 1,
        FieldValue::Int64(_) | FieldValue::Uint64(_) | FieldValue::Float64(_) => 8,
        FieldValue::String(value) | FieldValue::Enum(value) => {
            if value.len() > limits.max_field_bytes {
                return Err(QueryError::InvalidLimit);
            }
            value.len()
        }
        FieldValue::List(items) => {
            if items.len() > limits.max_fields_per_row {
                return Err(QueryError::InvalidLimit);
            }
            for item in items.iter() {
                admit_field_value(item, depth.saturating_add(1), limits, values, bytes)?;
            }
            0
        }
        _ => return Err(QueryError::InvalidLimit),
    };
    *bytes = bytes
        .checked_add(own_bytes)
        .filter(|total| *total <= limits.max_total_bytes)
        .ok_or(QueryError::InvalidLimit)?;
    Ok(())
}

/// One query row inseparably carrying the exact request identity.
#[derive(Clone, Debug)]
pub struct BoundSemanticQueryRow {
    identity: Arc<SemanticQueryIdentity>,
    row: trustfall::QueryResult,
}

impl BoundSemanticQueryRow {
    /// Returns the exact request identity carried by this row.
    #[must_use]
    pub fn identity(&self) -> &SemanticQueryIdentity {
        &self.identity
    }

    /// Borrows the Trustfall result row.
    #[must_use]
    pub const fn row(&self) -> &trustfall::QueryResult {
        &self.row
    }

    /// Consumes the binding and returns the Trustfall result row.
    #[must_use]
    pub fn into_row(self) -> trustfall::QueryResult {
        self.row
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SemanticQueryTerminalState {
    Complete,
    LimitReached,
    Cancelled,
}

/// Fused terminal tied to the same typed evidence identity as every row.
#[derive(Clone, Debug)]
pub struct SemanticQueryTerminal {
    identity: Arc<SemanticQueryIdentity>,
    state: SemanticQueryTerminalState,
    rows: usize,
}

impl SemanticQueryTerminal {
    /// Returns the exact request identity carried by this terminal.
    #[must_use]
    pub fn identity(&self) -> &SemanticQueryIdentity {
        &self.identity
    }

    /// Returns the number of rows emitted before this terminal.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// Reports a fully exhausted query.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, SemanticQueryTerminalState::Complete)
    }

    /// Reports cancellation before complete exhaustion.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.state, SemanticQueryTerminalState::Cancelled)
    }

    /// Reports that another bounded page is available.
    #[must_use]
    pub const fn is_limit_reached(&self) -> bool {
        matches!(self.state, SemanticQueryTerminalState::LimitReached)
    }
}

/// One row or the single fused terminal from a semantic query stream.
#[derive(Clone, Debug)]
pub enum SemanticQueryEvent {
    /// A result tied to the exact admitted request identity.
    Row(BoundSemanticQueryRow),
    /// The stream's single terminal state.
    Terminal(SemanticQueryTerminal),
}

/// Lazy bounded events over one immutable semantic evidence corpus.
pub struct SemanticQueryStream {
    rows: QueryResultStream<'static>,
    identity: Arc<SemanticQueryIdentity>,
    cancellation: SemanticQueryCancellation,
    emitted: usize,
    skipped: usize,
    start_offset: usize,
    terminal_emitted: bool,
}

impl SemanticQueryStream {
    /// Returns the exact request identity shared by all emitted events.
    #[must_use]
    pub fn identity(&self) -> &SemanticQueryIdentity {
        &self.identity
    }
}

impl Stream for SemanticQueryStream {
    type Item = SemanticQueryEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.terminal_emitted {
            return Poll::Ready(None);
        }
        if self.cancellation.is_cancelled() {
            self.terminal_emitted = true;
            return Poll::Ready(Some(SemanticQueryEvent::Terminal(SemanticQueryTerminal {
                identity: Arc::clone(&self.identity),
                state: SemanticQueryTerminalState::Cancelled,
                rows: self.emitted,
            })));
        }
        loop {
            match self.rows.as_mut().poll_next(context) {
                Poll::Ready(Some(row)) => {
                    if self.skipped < self.start_offset {
                        self.skipped += 1;
                        continue;
                    }
                    if self.emitted == self.identity.limit {
                        self.terminal_emitted = true;
                        return Poll::Ready(Some(SemanticQueryEvent::Terminal(
                            SemanticQueryTerminal {
                                identity: Arc::clone(&self.identity),
                                state: SemanticQueryTerminalState::LimitReached,
                                rows: self.emitted,
                            },
                        )));
                    }
                    self.emitted += 1;
                    return Poll::Ready(Some(SemanticQueryEvent::Row(BoundSemanticQueryRow {
                        identity: Arc::clone(&self.identity),
                        row,
                    })));
                }
                Poll::Ready(None) => {
                    self.terminal_emitted = true;
                    let state = if self.cancellation.is_cancelled() {
                        SemanticQueryTerminalState::Cancelled
                    } else {
                        SemanticQueryTerminalState::Complete
                    };
                    return Poll::Ready(Some(SemanticQueryEvent::Terminal(
                        SemanticQueryTerminal {
                            identity: Arc::clone(&self.identity),
                            state,
                            rows: self.emitted,
                        },
                    )));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Parses and returns the process-wide immutable semantic query schema.
pub fn schema() -> Result<&'static Schema, QueryError> {
    PARSED_SCHEMA
        .get_or_init(|| Schema::parse(SCHEMA).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| QueryError::Schema(error.clone()))
}

/// Executes a previously admitted request as a lazy, bounded, fused stream.
pub fn execute_semantic_query(
    request: SemanticQueryRequest,
) -> Result<SemanticQueryStream, QueryError> {
    if request.cancellation.is_cancelled() {
        return Err(QueryError::Cancelled);
    }
    let adapter = Arc::new(SemanticAdapter::new(request.corpus));
    let rows = trustfall::execute_query_async(
        schema()?,
        adapter,
        request.identity.query(),
        request.identity.variables().clone(),
    )
    .map_err(|error| QueryError::Query(error.to_string()))?;
    Ok(SemanticQueryStream {
        rows,
        identity: request.identity,
        cancellation: request.cancellation,
        emitted: 0,
        skipped: 0,
        start_offset: request.start_offset,
        terminal_emitted: false,
    })
}

#[derive(Debug)]
struct SemanticGraph {
    corpus: SemanticQueryCorpus,
    all: Arc<[usize]>,
    projects: Arc<[usize]>,
    declarations: Arc<[usize]>,
    external_targets: Arc<[usize]>,
    by_id: BTreeMap<String, usize>,
    children: BTreeMap<String, Arc<[usize]>>,
    referenced_by: BTreeMap<String, Arc<[usize]>>,
    project_members: BTreeMap<PackageKey, Arc<[usize]>>,
}

impl SemanticGraph {
    fn new(corpus: SemanticQueryCorpus) -> Self {
        let mut projects = Vec::new();
        let mut declarations = Vec::new();
        let mut external_targets = Vec::new();
        let mut by_id = BTreeMap::new();
        let mut children = BTreeMap::<String, Vec<usize>>::new();
        let mut referenced_by = BTreeMap::<String, Vec<usize>>::new();
        let mut project_members = BTreeMap::<PackageKey, Vec<usize>>::new();
        for (index, fact) in corpus.facts().iter().enumerate() {
            let row = fact.presentation();
            by_id.insert(row.id.clone(), index);
            match fact.evidence() {
                SemanticQueryEvidence::Package(_) => projects.push(index),
                SemanticQueryEvidence::CompilerExternalTarget(_) => {
                    external_targets.push(index);
                }
                SemanticQueryEvidence::Compiler(_)
                | SemanticQueryEvidence::StructuralFallback(_) => declarations.push(index),
            }
            project_members
                .entry(fact.evidence().package())
                .or_default()
                .push(index);
            if let Some(parent) = &row.parent {
                children.entry(parent.clone()).or_default().push(index);
            } else if let Some(project) = &row.project {
                children.entry(project.clone()).or_default().push(index);
            }
            for target in &row.related {
                referenced_by.entry(target.clone()).or_default().push(index);
            }
        }
        Self {
            all: (0..corpus.facts().len()).collect::<Vec<_>>().into(),
            projects: projects.into(),
            declarations: declarations.into(),
            external_targets: external_targets.into(),
            by_id,
            children: children
                .into_iter()
                .map(|(key, values)| (key, values.into()))
                .collect(),
            referenced_by: referenced_by
                .into_iter()
                .map(|(key, values)| (key, values.into()))
                .collect(),
            project_members: project_members
                .into_iter()
                .map(|(key, values)| (key, values.into()))
                .collect(),
            corpus,
        }
    }
}

#[derive(Clone, Debug)]
struct Vertex {
    graph: Arc<SemanticGraph>,
    index: usize,
}

impl Vertex {
    fn fact(&self) -> &SemanticQueryFact {
        &self.graph.corpus.facts()[self.index]
    }

    fn one(&self, id: Option<&str>) -> Arc<[usize]> {
        id.and_then(|id| self.graph.by_id.get(id).copied())
            .map_or_else(|| Arc::from([]), |index| Arc::from([index]))
    }

    fn neighbors(&self, edge: &str) -> Arc<[usize]> {
        let fact = self.fact();
        let row = fact.presentation();
        match edge {
            "project" => self.one(row.project.as_deref()),
            "parent" => self.one(row.parent.as_deref()),
            "children" => self
                .graph
                .children
                .get(&row.id)
                .cloned()
                .unwrap_or_else(|| Arc::from([])),
            "sameProject" => self
                .graph
                .project_members
                .get(&fact.evidence().package())
                .cloned()
                .unwrap_or_else(|| Arc::from([])),
            "related" => row
                .related
                .iter()
                .filter_map(|id| self.graph.by_id.get(id).copied())
                .collect::<Vec<_>>()
                .into(),
            "referencedBy" => self
                .graph
                .referenced_by
                .get(&row.id)
                .cloned()
                .unwrap_or_else(|| Arc::from([])),
            _ => Arc::from([]),
        }
    }

    fn stream(
        graph: Arc<SemanticGraph>,
        indices: Arc<[usize]>,
    ) -> AsyncNeighborStream<'static, Self> {
        let length = indices.len();
        Box::pin(stream::iter((0..length).map(move |at| Self {
            graph: Arc::clone(&graph),
            index: indices[at],
        })))
    }
}

impl Typename for Vertex {
    fn typename(&self) -> &'static str {
        "CodeItem"
    }
}

#[derive(Debug)]
struct SemanticAdapter {
    graph: Arc<SemanticGraph>,
}

impl SemanticAdapter {
    fn new(corpus: SemanticQueryCorpus) -> Self {
        Self {
            graph: Arc::new(SemanticGraph::new(corpus)),
        }
    }
}

impl AsyncBasicAdapter<'static> for SemanticAdapter {
    type Vertex = Vertex;

    fn resolve_starting_vertices(
        &self,
        edge_name: &str,
        _parameters: &EdgeParameters,
    ) -> AsyncNeighborStream<'static, Self::Vertex> {
        let indices = match edge_name {
            "Item" => Arc::clone(&self.graph.all),
            "Project" => Arc::clone(&self.graph.projects),
            "Declaration" => Arc::clone(&self.graph.declarations),
            "ExternalTarget" => Arc::clone(&self.graph.external_targets),
            _ => Arc::from([]),
        };
        Vertex::stream(Arc::clone(&self.graph), indices)
    }

    fn resolve_property<V: AsVertex<Self::Vertex> + 'static>(
        &self,
        contexts: AsyncContextStream<'static, V>,
        _type_name: &str,
        property_name: &str,
    ) -> AsyncContextOutcomeStream<'static, V, FieldValue> {
        match property_name {
            "id" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().id.as_str().into()
            }),
            "kind" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().kind.as_str().into()
            }),
            "coordinate" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().coordinate.as_str().into()
            }),
            "name" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().name.as_str().into()
            }),
            "signature" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .fact()
                    .presentation()
                    .signature
                    .as_deref()
                    .map_or(FieldValue::Null, Into::into)
            }),
            "documentation" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().documentation.as_str().into()
            }),
            "score" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .fact()
                    .presentation()
                    .score
                    .map_or(FieldValue::Null, |score| u64::from(score).into())
            }),
            "state" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().evidence().state().into()
            }),
            "revision" => resolve_property_with(contexts, |vertex: &Vertex| {
                encode_hex(vertex.graph.corpus.workspace().as_bytes()).into()
            }),
            "profile" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .fact()
                    .evidence()
                    .profile()
                    .map_or(FieldValue::Null, |profile| profile_name(profile).into())
            }),
            "packageUrl" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .fact()
                    .evidence()
                    .package_url()
                    .map_or(FieldValue::Null, Into::into)
            }),
            _ => resolve_property_with(contexts, |_vertex: &Vertex| FieldValue::Null),
        }
    }

    fn resolve_neighbors<V: AsVertex<Self::Vertex> + 'static>(
        &self,
        contexts: AsyncContextStream<'static, V>,
        _type_name: &str,
        edge_name: &str,
        _parameters: &EdgeParameters,
    ) -> AsyncContextOutcomeStream<'static, V, AsyncNeighborStream<'static, Self::Vertex>> {
        match edge_name {
            "project" | "parent" | "children" | "related" | "referencedBy" | "sameProject" => {
                let edge = edge_name.to_owned();
                resolve_neighbors_with(contexts, move |vertex: &Vertex| {
                    Vertex::stream(Arc::clone(&vertex.graph), vertex.neighbors(&edge))
                })
            }
            _ => resolve_neighbors_with(contexts, |_vertex: &Vertex| Box::pin(stream::empty())),
        }
    }

    fn resolve_coercion<V: AsVertex<Self::Vertex> + 'static>(
        &self,
        contexts: AsyncContextStream<'static, V>,
        _type_name: &str,
        coerce_to_type: &str,
    ) -> AsyncContextOutcomeStream<'static, V, bool> {
        let allowed = coerce_to_type == "CodeItem";
        resolve_coercion_with(contexts, move |_vertex: &Vertex| allowed)
    }
}

fn profile_name(profile: LanguageProfile) -> String {
    let [language, variant] = <[u8; 2]>::from(profile);
    format!("{language}:{variant}")
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
