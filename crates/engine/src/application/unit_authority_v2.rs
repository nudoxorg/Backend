//! Exact source-unit admission and portable option snapshots for compiler V2.
//!
//! Callers supply captured bytes and canonical root-relative paths. Admission
//! checks that the language-native key selects those actual members and binds
//! every member path and byte sequence; the key digest alone is insufficient.

use crate::compiler_input_manifest_v2::{
    CompilationUnitKeyV2, CompilerInputManifestV2, CompilerPackageTargetV2,
    CompilerReadFrontierStatusV2, validate_compiler_input_path_v2,
};
use crate::compiler_input_tree_v2::{
    CompilerInputMerkleTreeV2, CompilerInputTreeKindV2, CompilerInputTreeRecordV2,
    CompilerWorkspaceFileRoleV2,
};
use backend_frontend_go::legacy::{GoOracleInvocationModeV1, GoOracleInvocationOptionsV1};
use backend_frontend_python::legacy::PyreflyInvocationOptionsV1;
use backend_semantic::vocabulary::{Language, LanguageProfile, Stage};
use thiserror::Error;

const KEY_DOMAIN: &[u8] = b"compiler.unit-key.v2\0";
const MEMBERS_DOMAIN: &[u8] = b"compiler.unit-members.v2\0";
const OPTIONS_DOMAIN: &[u8] = b"compiler.portable-options.v2\0";
const PLAN_DOMAIN: &[u8] = b"compiler.unit-plan.v2\0";
const WORKSPACE_SELECTION_DOMAIN: &[u8] = b"compiler.workspace-unit-selection.v2\0";

/// The semantic kind of one exact V2 compilation unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilationUnitKindV2 {
    /// One package-wide source set.
    PackageRoot,
    /// One Rust crate target and root source.
    RustCrate,
    /// One TypeScript program selected by its configuration path.
    TypeScriptProgram,
    /// One Python module or package directory.
    PythonModule,
    /// One Go package directory.
    GoPackage,
    /// One Java source set, represented by the current Java module key.
    JavaSourceSet,
    /// One C# project and project-directory source set.
    CSharpProject,
    /// One C or C++ translation unit.
    ClangTranslationUnit,
}

/// A source or configuration file from an already admitted capture.
#[derive(Clone, Copy, Debug)]
pub struct CapturedUnitMemberV2<'source> {
    relative_path: &'source str,
    bytes: &'source [u8],
}

impl<'source> CapturedUnitMemberV2<'source> {
    /// Admits one captured file under the canonical relative-path grammar.
    ///
    /// # Errors
    /// Returns InvalidPath when the path is noncanonical.
    pub fn new(
        relative_path: &'source str,
        bytes: &'source [u8],
    ) -> Result<Self, UnitAuthorityV2Error> {
        validate_compiler_input_path_v2(relative_path)
            .map_err(|_| UnitAuthorityV2Error::InvalidPath)?;
        Ok(Self {
            relative_path,
            bytes,
        })
    }

    /// Returns the canonical root-relative file path.
    #[must_use]
    pub const fn relative_path(self) -> &'source str {
        self.relative_path
    }

    /// Returns the exact captured file bytes.
    #[must_use]
    pub const fn bytes(self) -> &'source [u8] {
        self.bytes
    }
}

/// Typed unit-admission or portable-option construction failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum UnitAuthorityV2Error {
    /// A unit key or member has a noncanonical path.
    #[error("compilation unit contains a noncanonical path")]
    InvalidPath,
    /// A unit key contains an invalid native name.
    #[error("compilation unit contains an invalid native name")]
    InvalidName,
    /// A path does not have the file shape required by its key variant.
    #[error("compilation unit path has the wrong language-native file shape")]
    InvalidUnitShape,
    /// The selected profile disagrees with the key variant.
    #[error("compilation unit key does not agree with the language profile")]
    ProfileKeyMismatch,
    /// The captured unit has no members.
    #[error("captured compilation unit has no members")]
    EmptyMemberSet,
    /// Captured member paths are duplicated or not strictly ordered.
    #[error("captured members are duplicated or not in canonical path order")]
    MemberOrder,
    /// A captured member is outside the scope named by its key.
    #[error("captured member is outside the compilation unit scope")]
    MemberOutsideUnit,
    /// A key-selected config or source file is absent.
    #[error("compilation unit key names a missing captured member")]
    MissingUnitAnchor,
    /// The captured set has no source file for the selected language.
    #[error("captured unit has no source file for its language")]
    MissingLanguageSource,
    /// An option contains invalid text or an absolute host path.
    #[error("portable option contains an invalid or host-local value")]
    InvalidPortableOption,
    /// Planning needs a compiler closure not represented by this contract.
    #[error("unsupported compiler closure requirement: {0}")]
    UnsupportedClosure(UnsupportedClosureRequirementV2),
    /// The supplied page tree is not a verified full-workspace tree.
    #[error("compilation unit selection requires a full-workspace tree")]
    InvalidWorkspaceTree,
    /// The key, profile, stage, or target differs from the selected unit.
    #[error("workspace unit selection does not match its typed target or recipe")]
    SelectionBindingMismatch,
    /// A project/configuration anchor has the wrong workspace file role.
    #[error("compilation unit anchor has the wrong workspace file role")]
    AnchorRoleMismatch,
    /// The option snapshot belongs to another language.
    #[error("portable option snapshot does not match the admitted unit language")]
    OptionLanguageMismatch,
}

/// Compiler closure requirement that cannot yet be represented portably.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum UnsupportedClosureRequirementV2 {
    /// The complete TypeScript module and configuration tree is unavailable.
    #[error("TypeScript module tree")]
    TypeScriptModuleTree,
    /// A separately installed Go oracle binary has no portable source closure.
    #[error("Go oracle binary")]
    GoOracleBinary,
    /// External Java classpath contents are not captured.
    #[error("Java external classpath")]
    JavaExternalClasspath,
    /// Python imports and runtime module discovery are not captured.
    #[error("Python import closure")]
    PythonImportClosure,
    /// Go package imports, workspace/module resolution, and source closure are not captured.
    #[error("Go package import closure")]
    GoPackageImportClosure,
    /// Clang include search, headers, and toolchain-provided include closure are not captured.
    #[error("Clang include closure")]
    ClangIncludeClosure,
}

/// One source-file identity selected from a verified workspace inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedUnitSourceV2 {
    relative_path: Box<str>,
    object_id: [u8; 32],
    length: u64,
}

impl SelectedUnitSourceV2 {
    /// Returns the normalized workspace-relative path.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Returns the content-addressed identity of the captured bytes.
    #[must_use]
    pub const fn object_id(&self) -> [u8; 32] {
        self.object_id
    }

    /// Returns the exact captured byte length.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }
}

/// Deterministic source candidates selected from one complete workspace inventory.
///
/// This proves only path/role selection against the committed workspace tree.
/// It does not prove compiler reads, imports, includes, classpaths, or other
/// dynamic closure requirements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedUnitSourcesV2 {
    package_target: CompilerPackageTargetV2,
    profile: LanguageProfile,
    stage: Stage,
    kind: CompilationUnitKindV2,
    workspace_root: [u8; 32],
    anchor: Option<SelectedUnitSourceV2>,
    sources: Box<[SelectedUnitSourceV2]>,
    selection_digest: [u8; 32],
}

impl SelectedUnitSourcesV2 {
    /// Returns the package and exact native-unit target.
    #[must_use]
    pub const fn package_target(&self) -> &CompilerPackageTargetV2 {
        &self.package_target
    }

    /// Returns the closed compiler profile bound to this selection.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    /// Returns the compiler stage bound to this selection.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.stage
    }

    /// Returns the semantic unit kind.
    #[must_use]
    pub const fn kind(&self) -> CompilationUnitKindV2 {
        self.kind
    }

    /// Returns the verified workspace-tree root from which candidates were selected.
    #[must_use]
    pub const fn workspace_root(&self) -> [u8; 32] {
        self.workspace_root
    }

    /// Returns the exact config/project/source anchor, if this key names one.
    #[must_use]
    pub const fn anchor(&self) -> Option<&SelectedUnitSourceV2> {
        self.anchor.as_ref()
    }

    /// Returns selected source members in canonical path order.
    #[must_use]
    pub fn sources(&self) -> &[SelectedUnitSourceV2] {
        &self.sources
    }

    /// Returns the digest over target, workspace root, selectors, anchor, and sources.
    #[must_use]
    pub const fn selection_digest(&self) -> [u8; 32] {
        self.selection_digest
    }
}

/// A frontend availability fact supplied by the worker's local registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnitFrontendAvailabilityV2 {
    /// The exact frontend is configured and available on this worker.
    Available,
    /// No exact frontend is configured for this target/profile.
    Unavailable,
}

/// Concrete reason remote execution must fall back to the local compiler.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum UnitExecutionFallbackReasonV2 {
    /// The local runtime did not provide an exact execution identity.
    #[error("exact local execution identity is unavailable")]
    ExecutionIdentityUnavailable,
    /// The runtime toolchain, environment, platform, options, or recipe differs.
    #[error("exact local execution identity differs from the admitted manifest")]
    ExecutionIdentityMismatch,
    /// No exact frontend is available for the requested language profile.
    #[error("exact frontend is unavailable for {0:?}")]
    FrontendUnavailable(Language),
    /// The workspace tree is not a compiler read-frontier completeness proof.
    #[error("complete compiler read frontier was not captured")]
    ReadFrontierNotCaptured,
    /// The language needs a closure the current V2 capture cannot attest.
    #[error("unsupported compiler closure requirement: {0}")]
    UnsupportedClosure(UnsupportedClosureRequirementV2),
}

/// Current decision for an otherwise well-bound unit selection.
///
/// `ExistingNativeRoute` is only a signal to preserve the worker's existing
/// Rust/C#/Rust-or-C#-PackageRoot checks. It is not a new remote readiness proof
/// and must never bypass that existing admission path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnitExecutionReadinessV2 {
    /// Keep using the worker's existing exact native-unit admission checks.
    ExistingNativeRoute,
    /// Keep this unit on the local compiler path for the reported reason.
    LocalFallback(UnitExecutionFallbackReasonV2),
}

/// Opaque proof that an exact V2 key matches the supplied captured source set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedCompilationUnitV2 {
    key: CompilationUnitKeyV2,
    kind: CompilationUnitKindV2,
    language: Language,
    member_count: u64,
    key_digest: [u8; 32],
    member_set_digest: [u8; 32],
}

impl AdmittedCompilationUnitV2 {
    /// Returns the exact admitted key.
    #[must_use]
    pub const fn key(&self) -> &CompilationUnitKeyV2 {
        &self.key
    }

    /// Returns the semantic kind.
    #[must_use]
    pub const fn kind(&self) -> CompilationUnitKindV2 {
        self.kind
    }

    /// Returns the selected language family.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    /// Returns the exact captured member count.
    #[must_use]
    pub const fn member_count(&self) -> u64 {
        self.member_count
    }

    /// Returns the portable unit-key digest.
    #[must_use]
    pub const fn key_digest(&self) -> [u8; 32] {
        self.key_digest
    }

    /// Returns the digest over every member path and byte sequence.
    #[must_use]
    pub const fn member_set_digest(&self) -> [u8; 32] {
        self.member_set_digest
    }
}

/// Admits a native V2 key against its exact captured files.
///
/// Members must be strictly ordered by canonical path. Their bytes and paths
/// are hashed after key, profile, anchor, source-kind, and scope checks. This
/// admits the source set supplied by the caller; it does not assert dynamic
/// dependency completeness.
///
/// # Errors
/// Returns a typed error for mismatched profiles, malformed keys, absent
/// anchors, out-of-scope/empty sets, or unordered members.
pub fn admit_compilation_unit_v2(
    key: &CompilationUnitKeyV2,
    profile: Language,
    members: &[CapturedUnitMemberV2<'_>],
) -> Result<AdmittedCompilationUnitV2, UnitAuthorityV2Error> {
    if members.is_empty() {
        return Err(UnitAuthorityV2Error::EmptyMemberSet);
    }
    if members
        .windows(2)
        .any(|pair| pair[0].relative_path >= pair[1].relative_path)
    {
        return Err(UnitAuthorityV2Error::MemberOrder);
    }

    let (kind, key_language, anchor, scope) = describe_key(key)?;
    if kind != CompilationUnitKindV2::PackageRoot && key_language != profile {
        return Err(UnitAuthorityV2Error::ProfileKeyMismatch);
    }
    let language = if kind == CompilationUnitKindV2::PackageRoot {
        profile
    } else {
        key_language
    };
    let mut anchor_found = anchor.is_none();
    let mut source_found = false;
    let mut members_hash = blake3::Hasher::new();
    members_hash.update(MEMBERS_DOMAIN);
    members_hash.update(&(members.len() as u64).to_be_bytes());

    for member in members {
        validate_compiler_input_path_v2(member.relative_path)
            .map_err(|_| UnitAuthorityV2Error::InvalidPath)?;
        if !unit_source_candidate_in_scope(kind, member.relative_path, scope) {
            return Err(UnitAuthorityV2Error::MemberOutsideUnit);
        }
        anchor_found |= anchor == Some(member.relative_path);
        source_found |= is_language_source(kind, language, member.relative_path);
        hash_field(&mut members_hash, member.relative_path.as_bytes());
        hash_field(&mut members_hash, member.bytes);
    }
    if !anchor_found {
        return Err(UnitAuthorityV2Error::MissingUnitAnchor);
    }
    if !source_found {
        return Err(UnitAuthorityV2Error::MissingLanguageSource);
    }

    Ok(AdmittedCompilationUnitV2 {
        key: key.clone(),
        kind,
        language,
        member_count: members.len() as u64,
        key_digest: digest_key(key)?,
        member_set_digest: *members_hash.finalize().as_bytes(),
    })
}

/// Selects deterministic source candidates from a verified full-workspace tree.
///
/// This validates the exact package target, profile, stage, native anchor, and
/// source paths/roles. A workspace inventory is not a compiler read frontier;
/// callers must use [`unit_execution_readiness_v2`] before remote admission.
///
/// # Errors
/// Returns a typed error for a wrong tree kind, profile/key mismatch, absent
/// or incorrectly-role-tagged anchor, or missing source members.
pub fn select_unit_sources_from_workspace_tree_v2(
    package_target: &CompilerPackageTargetV2,
    profile: LanguageProfile,
    stage: Stage,
    tree: &CompilerInputMerkleTreeV2,
) -> Result<SelectedUnitSourcesV2, UnitAuthorityV2Error> {
    if tree.kind() != CompilerInputTreeKindV2::Workspace {
        return Err(UnitAuthorityV2Error::InvalidWorkspaceTree);
    }
    let key = package_target.unit_key();
    let (kind, key_language, anchor_path, described_scope) = describe_key(key)?;
    if kind != CompilationUnitKindV2::PackageRoot && key_language != profile.language() {
        return Err(UnitAuthorityV2Error::ProfileKeyMismatch);
    }
    let language = if kind == CompilationUnitKindV2::PackageRoot {
        profile.language()
    } else {
        key_language
    };
    let scope = match (kind, key, described_scope) {
        (
            CompilationUnitKindV2::ClangTranslationUnit,
            CompilationUnitKeyV2::ClangTranslationUnit { source_path, .. },
            _,
        ) => UnitScope::Exact(source_path),
        (_, _, scope) => scope,
    };

    let mut anchor = None;
    let mut sources = Vec::new();
    for page in tree.pages() {
        let CompilerInputTreeRecordV2::File {
            path,
            role,
            object_id,
            length,
        } = page.record()
        else {
            continue;
        };
        validate_path(path)?;
        if Some(path.as_ref()) == anchor_path {
            let required_role = match kind {
                CompilationUnitKindV2::TypeScriptProgram | CompilationUnitKindV2::CSharpProject => {
                    CompilerWorkspaceFileRoleV2::Configuration
                }
                CompilationUnitKindV2::RustCrate
                | CompilationUnitKindV2::PythonModule
                | CompilationUnitKindV2::ClangTranslationUnit => {
                    CompilerWorkspaceFileRoleV2::Source
                }
                CompilationUnitKindV2::PackageRoot
                | CompilationUnitKindV2::GoPackage
                | CompilationUnitKindV2::JavaSourceSet => *role,
            };
            if *role != required_role {
                return Err(UnitAuthorityV2Error::AnchorRoleMismatch);
            }
            anchor = Some(SelectedUnitSourceV2 {
                relative_path: path.clone(),
                object_id: *object_id,
                length: *length,
            });
        }
        if *role == CompilerWorkspaceFileRoleV2::Source
            && unit_source_candidate_in_scope(kind, path, scope)
            && is_language_source(kind, language, path)
        {
            sources.push(SelectedUnitSourceV2 {
                relative_path: path.clone(),
                object_id: *object_id,
                length: *length,
            });
        }
    }

    if anchor_path.is_some() && anchor.is_none() {
        return Err(UnitAuthorityV2Error::MissingUnitAnchor);
    }
    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if sources.is_empty() {
        return Err(UnitAuthorityV2Error::MissingLanguageSource);
    }
    if sources
        .windows(2)
        .any(|pair| pair[0].relative_path >= pair[1].relative_path)
    {
        return Err(UnitAuthorityV2Error::MemberOrder);
    }

    let key_digest = digest_key(key)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(WORKSPACE_SELECTION_DOMAIN);
    hasher.update(package_target.target().as_ref());
    hasher.update(&tree.root());
    hasher.update(&<[u8; 2]>::from(profile));
    hasher.update(&[u8::from(stage), unit_kind_tag(kind)]);
    hasher.update(&key_digest);
    if let Some(anchor_member) = &anchor {
        hasher.update(&[1]);
        hash_selected_source(&mut hasher, anchor_member);
    } else {
        hasher.update(&[0]);
    }
    hasher.update(&(sources.len() as u64).to_be_bytes());
    for source in &sources {
        hash_selected_source(&mut hasher, source);
    }

    Ok(SelectedUnitSourcesV2 {
        package_target: package_target.clone(),
        profile,
        stage,
        kind,
        workspace_root: tree.root(),
        anchor,
        sources: sources.into_boxed_slice(),
        selection_digest: *hasher.finalize().as_bytes(),
    })
}

/// Returns the known, language-specific closure gap for a newly selected unit.
///
/// Go's oracle-binary mode is reported by
/// [`PortableOptionSnapshotV2::from_go_oracle_options`]; this table reports the
/// independent package/import closure requirement. `PackageRoot` is
/// profile-dependent and therefore has no single result here.
#[must_use]
pub const fn unit_remote_closure_requirement_v2(
    kind: CompilationUnitKindV2,
) -> Option<UnsupportedClosureRequirementV2> {
    match kind {
        CompilationUnitKindV2::TypeScriptProgram => {
            Some(UnsupportedClosureRequirementV2::TypeScriptModuleTree)
        }
        CompilationUnitKindV2::PythonModule => {
            Some(UnsupportedClosureRequirementV2::PythonImportClosure)
        }
        CompilationUnitKindV2::GoPackage => {
            Some(UnsupportedClosureRequirementV2::GoPackageImportClosure)
        }
        CompilationUnitKindV2::JavaSourceSet => {
            Some(UnsupportedClosureRequirementV2::JavaExternalClasspath)
        }
        CompilationUnitKindV2::ClangTranslationUnit => {
            Some(UnsupportedClosureRequirementV2::ClangIncludeClosure)
        }
        CompilationUnitKindV2::PackageRoot
        | CompilationUnitKindV2::RustCrate
        | CompilationUnitKindV2::CSharpProject => None,
    }
}

/// Checks target, profile, stage, frontend, toolchain, environment, platform,
/// and portable options before returning the current fail-closed decision.
///
/// The full-workspace manifest's read identity is deliberately not treated as
/// a compiler-read completeness assertion. Even when all runtime fields match,
/// this V2 contract keeps the unit local until a trusted complete read frontier
/// can be supplied. Cross-language keys return their specific closure gap.
/// Exact Rust/C# routes return `ExistingNativeRoute` only to preserve their
/// current worker admission checks; that marker is not sufficient to Accept
/// or execute a job on its own.
#[must_use]
pub fn unit_execution_readiness_v2(
    selection: &SelectedUnitSourcesV2,
    manifest: &CompilerInputManifestV2,
    execution_identity: Option<super::runtime::LocalCompilerExecutionIdentity>,
    frontend: UnitFrontendAvailabilityV2,
) -> Result<UnitExecutionReadinessV2, UnitAuthorityV2Error> {
    if manifest.package_target() != &selection.package_target
        || manifest.profile() != selection.profile
        || manifest.stage() != selection.stage
        || manifest.workspace_root() != selection.workspace_root
    {
        return Err(UnitAuthorityV2Error::SelectionBindingMismatch);
    }
    let Some(identity) = execution_identity else {
        return Ok(UnitExecutionReadinessV2::LocalFallback(
            UnitExecutionFallbackReasonV2::ExecutionIdentityUnavailable,
        ));
    };
    let recipe = manifest.invocation_recipe();
    if identity.target() != selection.package_target.target()
        || identity.profile() != selection.profile
        || identity.stage() != selection.stage
        || identity.invocation_recipe() != recipe
        || identity.toolchain_identity() != manifest.toolchain()
        || identity.environment_identity() != manifest.environment()
        || identity.target_platform_identity() != manifest.target_platform()
    {
        return Ok(UnitExecutionReadinessV2::LocalFallback(
            UnitExecutionFallbackReasonV2::ExecutionIdentityMismatch,
        ));
    }
    if frontend == UnitFrontendAvailabilityV2::Unavailable {
        return Ok(UnitExecutionReadinessV2::LocalFallback(
            UnitExecutionFallbackReasonV2::FrontendUnavailable(selection.profile.language()),
        ));
    }
    if is_existing_native_unit(selection.kind, selection.profile.language()) {
        return Ok(UnitExecutionReadinessV2::ExistingNativeRoute);
    }
    match manifest.read_frontier_status() {
        CompilerReadFrontierStatusV2::Unproven => {
            if let Some(requirement) =
                unit_closure_for_language(selection.kind, selection.profile.language())
            {
                return Ok(UnitExecutionReadinessV2::LocalFallback(
                    UnitExecutionFallbackReasonV2::UnsupportedClosure(requirement),
                ));
            }
            Ok(UnitExecutionReadinessV2::LocalFallback(
                UnitExecutionFallbackReasonV2::ReadFrontierNotCaptured,
            ))
        }
    }
}

fn is_existing_native_unit(kind: CompilationUnitKindV2, language: Language) -> bool {
    matches!(
        (kind, language),
        (CompilationUnitKindV2::RustCrate, Language::Rust)
            | (CompilationUnitKindV2::CSharpProject, Language::CSharp)
            | (
                CompilationUnitKindV2::PackageRoot,
                Language::Rust | Language::CSharp
            )
    )
}

fn unit_closure_for_language(
    kind: CompilationUnitKindV2,
    language: Language,
) -> Option<UnsupportedClosureRequirementV2> {
    unit_remote_closure_requirement_v2(kind).or_else(|| match (kind, language) {
        (CompilationUnitKindV2::PackageRoot, Language::TypeScript) => {
            Some(UnsupportedClosureRequirementV2::TypeScriptModuleTree)
        }
        (CompilationUnitKindV2::PackageRoot, Language::Python) => {
            Some(UnsupportedClosureRequirementV2::PythonImportClosure)
        }
        (CompilationUnitKindV2::PackageRoot, Language::Go) => {
            Some(UnsupportedClosureRequirementV2::GoPackageImportClosure)
        }
        (CompilationUnitKindV2::PackageRoot, Language::Java) => {
            Some(UnsupportedClosureRequirementV2::JavaExternalClasspath)
        }
        (CompilationUnitKindV2::PackageRoot, Language::Clang) => {
            Some(UnsupportedClosureRequirementV2::ClangIncludeClosure)
        }
        _ => None,
    })
}

fn hash_selected_source(hasher: &mut blake3::Hasher, source: &SelectedUnitSourceV2) {
    hash_field(hasher, source.relative_path.as_bytes());
    hasher.update(&source.object_id);
    hasher.update(&source.length.to_be_bytes());
}

/// Portable, path-independent snapshot of one language's compiler options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortableOptionSnapshotV2 {
    language: Language,
    canonical: Box<[u8]>,
    digest: [u8; 32],
}

impl PortableOptionSnapshotV2 {
    /// Snapshots ordered arguments from the existing Pyrefly option getter.
    ///
    /// # Errors
    /// Returns InvalidPortableOption for invalid text or absolute host paths.
    pub fn from_pyrefly_options(
        options: PyreflyInvocationOptionsV1<'_>,
    ) -> Result<Self, UnitAuthorityV2Error> {
        Self::from_ordered_arguments(Language::Python, options.arguments())
    }

    /// Snapshots the existing Go-oracle option getter, rejecting binary mode.
    ///
    /// # Errors
    /// Returns UnsupportedClosure(GoOracleBinary) for a separately installed
    /// oracle binary.
    pub fn from_go_oracle_options(
        options: GoOracleInvocationOptionsV1,
    ) -> Result<Self, UnitAuthorityV2Error> {
        if options.mode() == GoOracleInvocationModeV1::OracleBinary {
            return Err(UnitAuthorityV2Error::UnsupportedClosure(
                UnsupportedClosureRequirementV2::GoOracleBinary,
            ));
        }
        let identity =
            options
                .helper_source_identity()
                .ok_or(UnitAuthorityV2Error::UnsupportedClosure(
                    UnsupportedClosureRequirementV2::GoOracleBinary,
                ))?;
        let mut bytes = Vec::with_capacity(1 + identity.len());
        bytes.push(1); // Vendored oracle source run by the Go toolchain.
        bytes.extend_from_slice(&identity);
        Ok(Self::from_canonical(Language::Go, bytes))
    }

    /// Snapshots the Java release when no external classpath is required.
    ///
    /// # Errors
    /// Returns UnsupportedClosure(JavaExternalClasspath) when external
    /// classpath contents are required.
    pub fn for_java_release(
        release: &str,
        has_external_classpath: bool,
    ) -> Result<Self, UnitAuthorityV2Error> {
        if has_external_classpath {
            return Err(UnitAuthorityV2Error::UnsupportedClosure(
                UnsupportedClosureRequirementV2::JavaExternalClasspath,
            ));
        }
        validate_portable_text(release)?;
        let mut bytes = vec![1];
        append_field(&mut bytes, release.as_bytes());
        Ok(Self::from_canonical(Language::Java, bytes))
    }

    /// Snapshots ordered path-independent arguments from another frontend.
    ///
    /// Absolute POSIX and Windows paths are rejected before options are stored.
    ///
    /// # Errors
    /// Returns InvalidPortableOption for invalid text or host-local paths.
    pub fn from_ordered_arguments(
        language: Language,
        arguments: &[String],
    ) -> Result<Self, UnitAuthorityV2Error> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(arguments.len() as u64).to_be_bytes());
        for argument in arguments {
            validate_portable_text(argument)?;
            append_field(&mut bytes, argument.as_bytes());
        }
        Ok(Self::from_canonical(language, bytes))
    }

    /// Creates an empty option snapshot for a frontend with no explicit options.
    #[must_use]
    pub fn empty(language: Language) -> Self {
        Self::from_canonical(language, 0_u64.to_be_bytes().to_vec())
    }

    /// Returns the language family whose options were captured.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    /// Returns canonical path-independent option bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }

    /// Returns the portable snapshot digest.
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    fn from_canonical(language: Language, canonical: Vec<u8>) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(OPTIONS_DOMAIN);
        hasher.update(&[u8::from(language)]);
        hash_field(&mut hasher, &canonical);
        Self {
            language,
            canonical: canonical.into_boxed_slice(),
            digest: *hasher.finalize().as_bytes(),
        }
    }
}

/// Opaque plan binding one admitted unit and portable option snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilationUnitPlanV2 {
    kind: CompilationUnitKindV2,
    language: Language,
    key_digest: [u8; 32],
    member_set_digest: [u8; 32],
    options_digest: [u8; 32],
    digest: [u8; 32],
}

impl CompilationUnitPlanV2 {
    /// Returns the planned unit kind.
    #[must_use]
    pub const fn kind(&self) -> CompilationUnitKindV2 {
        self.kind
    }

    /// Returns the unit language.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    /// Returns the admitted key digest.
    #[must_use]
    pub const fn key_digest(&self) -> [u8; 32] {
        self.key_digest
    }

    /// Returns the admitted member-set digest.
    #[must_use]
    pub const fn member_set_digest(&self) -> [u8; 32] {
        self.member_set_digest
    }

    /// Returns the option snapshot digest.
    #[must_use]
    pub const fn options_digest(&self) -> [u8; 32] {
        self.options_digest
    }

    /// Returns the final portable plan digest.
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

/// Plans an admitted unit using its portable option snapshot.
///
/// TypeScript planning fails closed until its complete module tree is captured.
///
/// # Errors
/// Returns a typed language mismatch or unsupported-closure error.
pub fn plan_compilation_unit_v2(
    admitted: &AdmittedCompilationUnitV2,
    options: &PortableOptionSnapshotV2,
) -> Result<CompilationUnitPlanV2, UnitAuthorityV2Error> {
    if admitted.language != options.language {
        return Err(UnitAuthorityV2Error::OptionLanguageMismatch);
    }
    if admitted.kind == CompilationUnitKindV2::TypeScriptProgram {
        return Err(UnitAuthorityV2Error::UnsupportedClosure(
            UnsupportedClosureRequirementV2::TypeScriptModuleTree,
        ));
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(PLAN_DOMAIN);
    hasher.update(&[u8::from(admitted.language), unit_kind_tag(admitted.kind)]);
    hasher.update(&admitted.key_digest);
    hasher.update(&admitted.member_set_digest);
    hasher.update(&options.digest);
    Ok(CompilationUnitPlanV2 {
        kind: admitted.kind,
        language: admitted.language,
        key_digest: admitted.key_digest,
        member_set_digest: admitted.member_set_digest,
        options_digest: options.digest,
        digest: *hasher.finalize().as_bytes(),
    })
}

#[derive(Clone, Copy)]
enum UnitScope<'path> {
    All,
    Exact(&'path str),
    Directory(&'path str),
}

fn describe_key(
    key: &CompilationUnitKeyV2,
) -> Result<(CompilationUnitKindV2, Language, Option<&str>, UnitScope<'_>), UnitAuthorityV2Error> {
    let entry = match key {
        CompilationUnitKeyV2::PackageRoot => (
            CompilationUnitKindV2::PackageRoot,
            Language::Rust,
            None,
            UnitScope::All,
        ),
        CompilationUnitKeyV2::RustCrate { name, root } => {
            validate_name(name, false)?;
            validate_path(root.as_ref())?;
            (
                CompilationUnitKindV2::RustCrate,
                Language::Rust,
                Some(root.as_ref()),
                UnitScope::Directory(parent_path(root.as_ref()).unwrap_or("")),
            )
        }
        CompilationUnitKeyV2::TypeScriptProgram { config_path } => {
            validate_path(config_path.as_ref())?;
            if !has_extension(config_path, &["json"]) {
                return Err(UnitAuthorityV2Error::InvalidUnitShape);
            }
            (
                CompilationUnitKindV2::TypeScriptProgram,
                Language::TypeScript,
                Some(config_path.as_ref()),
                UnitScope::Directory(parent_path(config_path.as_ref()).unwrap_or("")),
            )
        }
        CompilationUnitKeyV2::PythonModule { root } => {
            validate_path(root.as_ref())?;
            if is_python_source(root.as_ref()) {
                (
                    CompilationUnitKindV2::PythonModule,
                    Language::Python,
                    Some(root.as_ref()),
                    UnitScope::Exact(root.as_ref()),
                )
            } else {
                (
                    CompilationUnitKindV2::PythonModule,
                    Language::Python,
                    None,
                    UnitScope::Directory(root.as_ref()),
                )
            }
        }
        CompilationUnitKeyV2::GoPackage { root } => {
            validate_path(root.as_ref())?;
            (
                CompilationUnitKindV2::GoPackage,
                Language::Go,
                None,
                UnitScope::Directory(root.as_ref()),
            )
        }
        CompilationUnitKeyV2::JavaModule { name, root } => {
            validate_name(name, true)?;
            validate_path(root.as_ref())?;
            (
                CompilationUnitKindV2::JavaSourceSet,
                Language::Java,
                None,
                UnitScope::Directory(root.as_ref()),
            )
        }
        CompilationUnitKeyV2::CSharpProject { project_path } => {
            validate_path(project_path.as_ref())?;
            if !has_extension(project_path, &["csproj"]) {
                return Err(UnitAuthorityV2Error::InvalidUnitShape);
            }
            (
                CompilationUnitKindV2::CSharpProject,
                Language::CSharp,
                Some(project_path.as_ref()),
                UnitScope::Directory(parent_path(project_path.as_ref()).unwrap_or("")),
            )
        }
        CompilationUnitKeyV2::ClangTranslationUnit {
            is_cxx,
            source_path,
        } => {
            validate_path(source_path.as_ref())?;
            if *is_cxx != is_cxx_source(source_path) {
                return Err(UnitAuthorityV2Error::ProfileKeyMismatch);
            }
            (
                CompilationUnitKindV2::ClangTranslationUnit,
                Language::Clang,
                Some(source_path.as_ref()),
                UnitScope::Directory(parent_path(source_path.as_ref()).unwrap_or("")),
            )
        }
    };
    Ok(entry)
}

fn validate_path(path: &str) -> Result<(), UnitAuthorityV2Error> {
    validate_compiler_input_path_v2(path).map_err(|_| UnitAuthorityV2Error::InvalidPath)
}

fn validate_name(name: &str, dotted: bool) -> Result<(), UnitAuthorityV2Error> {
    let allowed = |byte: u8| {
        byte.is_ascii_alphanumeric()
            || if dotted {
                matches!(byte, b'_' | b'.')
            } else {
                matches!(byte, b'_' | b'-')
            }
    };
    if name.is_empty()
        || name.len() > 4096
        || !name.is_ascii()
        || name.bytes().any(|byte| !allowed(byte))
        || (dotted && (name.starts_with('.') || name.ends_with('.') || name.contains("..")))
    {
        return Err(UnitAuthorityV2Error::InvalidName);
    }
    Ok(())
}

fn parent_path(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

fn member_in_scope(kind: CompilationUnitKindV2, path: &str, scope: UnitScope<'_>) -> bool {
    match (kind, scope) {
        (CompilationUnitKindV2::PackageRoot, UnitScope::All) => true,
        (_, UnitScope::Exact(exact)) => path == exact,
        (_, UnitScope::Directory(directory)) => {
            directory.is_empty()
                || path
                    .strip_prefix(directory)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        }
        _ => false,
    }
}

fn unit_source_candidate_in_scope(
    kind: CompilationUnitKindV2,
    path: &str,
    scope: UnitScope<'_>,
) -> bool {
    if kind == CompilationUnitKindV2::GoPackage {
        if let UnitScope::Directory(directory) = scope {
            return parent_path(path).unwrap_or("") == directory;
        }
    }
    member_in_scope(kind, path, scope)
}

fn is_language_source(kind: CompilationUnitKindV2, language: Language, path: &str) -> bool {
    match kind {
        CompilationUnitKindV2::PackageRoot => match language {
            Language::Rust => has_extension(path, &["rs"]),
            Language::TypeScript => has_extension(
                path,
                &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
            ),
            Language::Python => is_python_source(path),
            Language::Go => has_extension(path, &["go"]),
            Language::Java => has_extension(path, &["java"]),
            Language::CSharp => has_extension(path, &["cs"]),
            Language::Clang => has_extension(path, &["c", "cc", "cpp", "cxx"]),
        },
        CompilationUnitKindV2::RustCrate => has_extension(path, &["rs"]),
        CompilationUnitKindV2::TypeScriptProgram => has_extension(
            path,
            &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
        ),
        CompilationUnitKindV2::PythonModule => is_python_source(path),
        CompilationUnitKindV2::GoPackage => has_extension(path, &["go"]),
        CompilationUnitKindV2::JavaSourceSet => has_extension(path, &["java"]),
        CompilationUnitKindV2::CSharpProject => has_extension(path, &["cs"]),
        CompilationUnitKindV2::ClangTranslationUnit => {
            has_extension(path, &["c", "cc", "cpp", "cxx"])
        }
    }
}

fn has_extension(path: &str, extensions: &[&str]) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| extensions.contains(&extension))
}

fn is_python_source(path: &str) -> bool {
    has_extension(path, &["py", "pyi"])
}
fn is_cxx_source(path: &str) -> bool {
    has_extension(path, &["cc", "cpp", "cxx"])
}

fn digest_key(key: &CompilationUnitKeyV2) -> Result<[u8; 32], UnitAuthorityV2Error> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(KEY_DOMAIN);
    match key {
        CompilationUnitKeyV2::PackageRoot => {
            hasher.update(&[1]);
        }
        CompilationUnitKeyV2::RustCrate { name, root } => {
            validate_name(name, false)?;
            validate_path(root)?;
            hasher.update(&[2]);
            hash_field(&mut hasher, name.as_bytes());
            hash_field(&mut hasher, root.as_bytes());
        }
        CompilationUnitKeyV2::TypeScriptProgram { config_path } => {
            validate_path(config_path)?;
            hasher.update(&[3]);
            hash_field(&mut hasher, config_path.as_bytes());
        }
        CompilationUnitKeyV2::PythonModule { root } => {
            validate_path(root)?;
            hasher.update(&[4]);
            hash_field(&mut hasher, root.as_bytes());
        }
        CompilationUnitKeyV2::GoPackage { root } => {
            validate_path(root)?;
            hasher.update(&[5]);
            hash_field(&mut hasher, root.as_bytes());
        }
        CompilationUnitKeyV2::JavaModule { name, root } => {
            validate_name(name, true)?;
            validate_path(root)?;
            hasher.update(&[6]);
            hash_field(&mut hasher, name.as_bytes());
            hash_field(&mut hasher, root.as_bytes());
        }
        CompilationUnitKeyV2::CSharpProject { project_path } => {
            validate_path(project_path)?;
            hasher.update(&[7]);
            hash_field(&mut hasher, project_path.as_bytes());
        }
        CompilationUnitKeyV2::ClangTranslationUnit {
            is_cxx,
            source_path,
        } => {
            validate_path(source_path)?;
            if *is_cxx != is_cxx_source(source_path) {
                return Err(UnitAuthorityV2Error::ProfileKeyMismatch);
            }
            hasher.update(&[8, u8::from(*is_cxx)]);
            hash_field(&mut hasher, source_path.as_bytes());
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

fn unit_kind_tag(kind: CompilationUnitKindV2) -> u8 {
    match kind {
        CompilationUnitKindV2::PackageRoot => 1,
        CompilationUnitKindV2::RustCrate => 2,
        CompilationUnitKindV2::TypeScriptProgram => 3,
        CompilationUnitKindV2::PythonModule => 4,
        CompilationUnitKindV2::GoPackage => 5,
        CompilationUnitKindV2::JavaSourceSet => 6,
        CompilationUnitKindV2::CSharpProject => 7,
        CompilationUnitKindV2::ClangTranslationUnit => 8,
    }
}

fn hash_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn append_field(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}

fn validate_portable_text(value: &str) -> Result<(), UnitAuthorityV2Error> {
    if value.is_empty()
        || value.contains('\0')
        || value.chars().any(char::is_control)
        || contains_absolute_host_path(value)
    {
        return Err(UnitAuthorityV2Error::InvalidPortableOption);
    }
    Ok(())
}

fn contains_absolute_host_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    if value.contains('\\')
        || bytes.windows(3).any(|part| {
            part[0].is_ascii_alphabetic() && part[1] == b':' && matches!(part[2], b'/' | b'\\')
        })
    {
        return true;
    }
    bytes.iter().enumerate().any(|(index, byte)| {
        if *byte != b'/' {
            return false;
        }
        if index == 0
            || matches!(
                bytes[index - 1],
                b'=' | b':' | b',' | b';' | b'@' | b' ' | b'\t' | b'\n' | b'\'' | b'"'
            )
        {
            return true;
        }
        let token_start = bytes[..index]
            .iter()
            .rposition(|byte| byte.is_ascii_whitespace())
            .map_or(0, |start| start + 1);
        let option_prefix = &value[token_start..index];
        [
            "-I",
            "-L",
            "-F",
            "-isystem",
            "-iquote",
            "-idirafter",
            "-include",
            "-imacros",
            "-isysroot",
            "-iframework",
            "-fmodule-map-file",
        ]
        .iter()
        .any(|prefix| option_prefix == *prefix)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CapturedUnitMemberV2, CompilationUnitKindV2, PortableOptionSnapshotV2,
        UnitAuthorityV2Error, UnitExecutionFallbackReasonV2, UnitExecutionReadinessV2,
        UnitFrontendAvailabilityV2, UnsupportedClosureRequirementV2, admit_compilation_unit_v2,
        plan_compilation_unit_v2, select_unit_sources_from_workspace_tree_v2,
        unit_closure_for_language, unit_execution_readiness_v2, unit_remote_closure_requirement_v2,
    };
    use crate::compiler_input_manifest_v2::{
        CompilationUnitKeyV2, CompilerInputManifestV2, CompilerInvocationRecipeV2,
        CompilerPackageTargetV2,
    };
    use crate::compiler_input_tree_v2::{
        CompilerInputMerkleTreeV2, CompilerInputTreeKindV2, CompilerInputTreeRecordV2,
        CompilerWorkspaceFileRoleV2,
    };
    use backend_semantic::vocabulary::{
        CStandard, GoVersion, JavaRelease, Language, LanguageProfile, NativeTool, PackageUrl,
        PythonVersion, Stage, TypeScriptSource,
    };
    use backend_version::{ContentId, ToolchainDomain};

    fn member<'source>(path: &'source str, bytes: &'source [u8]) -> CapturedUnitMemberV2<'source> {
        CapturedUnitMemberV2::new(path, bytes).expect("canonical captured member")
    }

    fn target(unit: CompilationUnitKeyV2) -> CompilerPackageTargetV2 {
        let package = PackageUrl::parse("pkg:cargo/unit-fixture@1.0.0".to_owned())
            .expect("valid fixture package URL");
        if unit == CompilationUnitKeyV2::PackageRoot {
            CompilerPackageTargetV2::for_package(package)
        } else {
            CompilerPackageTargetV2::new(package, unit).expect("valid package target")
        }
    }

    fn workspace_tree(
        files: &[(&str, CompilerWorkspaceFileRoleV2, [u8; 32], u64)],
    ) -> CompilerInputMerkleTreeV2 {
        let mut directories = std::collections::BTreeSet::from([String::new()]);
        for (path, _, _, _) in files {
            for (index, _) in path.match_indices('/') {
                directories.insert(path[..index].to_owned());
            }
        }
        let mut records = directories
            .into_iter()
            .map(|path| CompilerInputTreeRecordV2::Directory {
                path: path.into_boxed_str(),
            })
            .collect::<Vec<_>>();
        records.extend(files.iter().map(|(path, role, object_id, length)| {
            CompilerInputTreeRecordV2::File {
                path: (*path).into(),
                role: *role,
                object_id: *object_id,
                length: *length,
            }
        }));
        records.sort_by(|left, right| left.path().cmp(right.path()));
        CompilerInputMerkleTreeV2::from_sorted_records(CompilerInputTreeKindV2::Workspace, records)
            .expect("canonical fixture workspace tree")
    }

    fn fixture_workspace_tree() -> CompilerInputMerkleTreeV2 {
        use CompilerWorkspaceFileRoleV2::{Configuration, Other, Source};

        workspace_tree(&[
            ("web/tsconfig.json", Configuration, [1; 32], 15),
            ("web/src/main.ts", Source, [2; 32], 26),
            ("outside.ts", Source, [3; 32], 13),
            ("python/pkg/__init__.py", Source, [4; 32], 0),
            ("python/pkg/app.py", Source, [5; 32], 10),
            ("python/pkg/stubs.pyi", Source, [6; 32], 7),
            ("python/elsewhere.py", Source, [7; 32], 8),
            ("go/pkg/main.go", Source, [8; 32], 14),
            ("go/pkg/nested/other.go", Source, [19; 32], 18),
            ("go/pkg/README.md", Other, [9; 32], 5),
            ("java/src/Main.java", Source, [10; 32], 26),
            ("java/src/Other.java", Source, [11; 32], 21),
            ("native/lib.rs", Source, [12; 32], 18),
            ("native/mod.rs", Source, [13; 32], 12),
            ("native/main.c", Source, [14; 32], 20),
            ("native/other.c", Source, [15; 32], 21),
            ("dotnet/app.csproj", Configuration, [16; 32], 12),
            ("dotnet/Program.cs", Source, [17; 32], 19),
            ("dotnet/Extra.cs", Source, [18; 32], 16),
        ])
    }

    fn manifest_for(
        package_target: CompilerPackageTargetV2,
        profile: LanguageProfile,
        tool: NativeTool,
        workspace_root: [u8; 32],
    ) -> CompilerInputManifestV2 {
        CompilerInputManifestV2::new(
            package_target,
            [20; 32],
            CompilerInvocationRecipeV2::new(
                profile,
                Stage::LowerIr,
                tool,
                ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture-toolchain"),
                [21; 32],
                [22; 32],
                [23; 32],
            )
            .expect("valid fixture invocation recipe"),
            [24; 32],
            "fixture-workspace-policy-v2",
            [25; 32],
            workspace_root,
            1024,
        )
        .expect("valid fixture manifest")
    }

    #[test]
    fn distinct_python_modules_bind_distinct_keys_and_member_sets() {
        let first_key = CompilationUnitKeyV2::PythonModule {
            root: "pkg/a.py".into(),
        };
        let second_key = CompilationUnitKeyV2::PythonModule {
            root: "pkg/b.py".into(),
        };
        let first = [member("pkg/a.py", b"value = 1\n")];
        let second = [member("pkg/b.py", b"value = 1\n")];
        let first = admit_compilation_unit_v2(&first_key, Language::Python, &first)
            .expect("first module admitted");
        let second = admit_compilation_unit_v2(&second_key, Language::Python, &second)
            .expect("second module admitted");
        let changed_source = [member("pkg/a.py", b"value = 2\n")];
        let changed = admit_compilation_unit_v2(&first_key, Language::Python, &changed_source)
            .expect("same module key admits changed captured source");
        assert_eq!(first.kind(), CompilationUnitKindV2::PythonModule);
        assert_ne!(first.key_digest(), second.key_digest());
        assert_ne!(first.member_set_digest(), second.member_set_digest());
        assert_eq!(first.key_digest(), changed.key_digest());
        assert_ne!(first.member_set_digest(), changed.member_set_digest());
    }

    #[test]
    fn wrong_or_missing_unit_members_fail_closed() {
        let key = CompilationUnitKeyV2::ClangTranslationUnit {
            is_cxx: false,
            source_path: "src/main.c".into(),
        };
        let wrong = [member("src/other.c", b"int main(void) { return 0; }\n")];
        assert_eq!(
            admit_compilation_unit_v2(&key, Language::Clang, &wrong),
            Err(UnitAuthorityV2Error::MissingUnitAnchor)
        );
        let missing = [];
        assert_eq!(
            admit_compilation_unit_v2(&key, Language::Clang, &missing),
            Err(UnitAuthorityV2Error::EmptyMemberSet)
        );
        let mismatch = [member("src/main.c", b"int main(void) { return 0; }\n")];
        assert_eq!(
            admit_compilation_unit_v2(&key, Language::Python, &mismatch),
            Err(UnitAuthorityV2Error::ProfileKeyMismatch)
        );
    }

    #[test]
    fn outside_package_members_and_key_mutations_fail_closed() {
        let key = CompilationUnitKeyV2::GoPackage {
            root: "cmd/tool".into(),
        };
        let valid = [member("cmd/tool/main.go", b"package main\n")];
        assert_eq!(
            admit_compilation_unit_v2(&key, Language::Go, &valid)
                .expect("Go package root selects its captured source")
                .kind(),
            CompilationUnitKindV2::GoPackage
        );
        let outside = [
            member("cmd/tool/main.go", b"package main\n"),
            member("cmd/tools/other.go", b"package tools\n"),
        ];
        assert_eq!(
            admit_compilation_unit_v2(&key, Language::Go, &outside),
            Err(UnitAuthorityV2Error::MemberOutsideUnit)
        );
    }

    #[test]
    fn rust_csharp_and_package_root_keys_match_their_selected_sources() {
        let rust_key = CompilationUnitKeyV2::RustCrate {
            name: "library".into(),
            root: "src/lib.rs".into(),
        };
        let rust_sources = [member("src/lib.rs", b"pub fn value() {}\n")];
        let rust = admit_compilation_unit_v2(&rust_key, Language::Rust, &rust_sources)
            .expect("Rust root matches the captured crate source");
        assert_eq!(rust.kind(), CompilationUnitKindV2::RustCrate);

        let csharp_key = CompilationUnitKeyV2::CSharpProject {
            project_path: "app/app.csproj".into(),
        };
        let csharp_sources = [
            member("app/app.csproj", b"<Project />\n"),
            member("app/program.cs", b"class Program {}\n"),
        ];
        let csharp = admit_compilation_unit_v2(&csharp_key, Language::CSharp, &csharp_sources)
            .expect("C# project and source match the captured project set");
        assert_eq!(csharp.kind(), CompilationUnitKindV2::CSharpProject);

        let java_key = CompilationUnitKeyV2::JavaModule {
            name: "com.example".into(),
            root: "src/main/java".into(),
        };
        let java_sources = [member(
            "src/main/java/Main.java",
            b"package com.example; class Main {}\n",
        )];
        let java = admit_compilation_unit_v2(&java_key, Language::Java, &java_sources)
            .expect("Java module key selects the exact source set");
        assert_eq!(java.kind(), CompilationUnitKindV2::JavaSourceSet);

        let root_sources = [member("src/Main.java", b"class Main {}\n")];
        let package_root = admit_compilation_unit_v2(
            &CompilationUnitKeyV2::PackageRoot,
            Language::Java,
            &root_sources,
        )
        .expect("package-root unit agrees with the selected Java profile");
        assert_eq!(package_root.language(), Language::Java);
        assert_eq!(package_root.kind(), CompilationUnitKindV2::PackageRoot);

        let csharp_wrong_profile = [
            member("app/app.csproj", b"<Project />\n"),
            member("app/program.cs", b"class Program {}\n"),
        ];
        assert_eq!(
            admit_compilation_unit_v2(&csharp_key, Language::Rust, &csharp_wrong_profile),
            Err(UnitAuthorityV2Error::ProfileKeyMismatch)
        );
    }

    #[test]
    fn typescript_module_tree_requirement_is_typed() {
        let key = CompilationUnitKeyV2::TypeScriptProgram {
            config_path: "web/tsconfig.json".into(),
        };
        let members = [
            member("web/src/main.ts", b"export const value = 1;\n"),
            member("web/tsconfig.json", b"{}\n"),
        ];
        let admitted = admit_compilation_unit_v2(&key, Language::TypeScript, &members)
            .expect("captured key and members agree");
        let options = PortableOptionSnapshotV2::empty(Language::TypeScript);
        assert_eq!(
            plan_compilation_unit_v2(&admitted, &options),
            Err(UnitAuthorityV2Error::UnsupportedClosure(
                UnsupportedClosureRequirementV2::TypeScriptModuleTree
            ))
        );
    }

    #[test]
    fn java_external_classpath_is_a_typed_unsupported_closure() {
        assert_eq!(
            PortableOptionSnapshotV2::for_java_release("21", true),
            Err(UnitAuthorityV2Error::UnsupportedClosure(
                UnsupportedClosureRequirementV2::JavaExternalClasspath
            ))
        );
    }

    #[test]
    fn go_oracle_binary_is_a_typed_unsupported_closure() {
        use backend_frontend_go::legacy::{GoOracle, GoOracleConfiguration};
        use std::path::PathBuf;

        let configuration =
            GoOracleConfiguration::oracle_binary(PathBuf::from("/opt/compiler/go-oracle"))
                .expect("absolute test oracle path");
        let oracle = GoOracle::default().with_configuration(configuration);
        assert_eq!(
            PortableOptionSnapshotV2::from_go_oracle_options(oracle.portable_invocation_options()),
            Err(UnitAuthorityV2Error::UnsupportedClosure(
                UnsupportedClosureRequirementV2::GoOracleBinary
            ))
        );
    }

    #[test]
    fn portable_option_snapshot_rejects_absolute_host_paths() {
        let arguments = vec!["--config=/Users/alice/project/pyproject.toml".to_owned()];
        assert_eq!(
            PortableOptionSnapshotV2::from_ordered_arguments(Language::Python, &arguments),
            Err(UnitAuthorityV2Error::InvalidPortableOption)
        );
    }

    #[test]
    fn workspace_selection_is_sorted_scoped_and_bound_to_actual_file_identities() {
        use CompilerWorkspaceFileRoleV2::{Configuration, Source};

        let tree = fixture_workspace_tree();
        let ts_target = target(CompilationUnitKeyV2::TypeScriptProgram {
            config_path: "web/tsconfig.json".into(),
        });
        let ts = select_unit_sources_from_workspace_tree_v2(
            &ts_target,
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            &tree,
        )
        .expect("TypeScript candidates selected from workspace inventory");
        assert_eq!(
            ts.anchor().expect("tsconfig anchor").relative_path(),
            "web/tsconfig.json"
        );
        assert_eq!(
            ts.sources()
                .iter()
                .map(|member| member.relative_path())
                .collect::<Vec<_>>(),
            vec!["web/src/main.ts"]
        );

        let python = select_unit_sources_from_workspace_tree_v2(
            &target(CompilationUnitKeyV2::PythonModule {
                root: "python/pkg".into(),
            }),
            LanguageProfile::Python(PythonVersion::Python314),
            Stage::LowerIr,
            &tree,
        )
        .expect("Python package candidates selected");
        assert_eq!(
            python
                .sources()
                .iter()
                .map(|member| member.relative_path())
                .collect::<Vec<_>>(),
            vec![
                "python/pkg/__init__.py",
                "python/pkg/app.py",
                "python/pkg/stubs.pyi"
            ]
        );

        let go = select_unit_sources_from_workspace_tree_v2(
            &target(CompilationUnitKeyV2::GoPackage {
                root: "go/pkg".into(),
            }),
            LanguageProfile::Go(GoVersion::Go125),
            Stage::LowerIr,
            &tree,
        )
        .expect("Go package candidates selected");
        assert_eq!(go.sources().len(), 1);
        assert_eq!(go.sources()[0].relative_path(), "go/pkg/main.go");

        let java = select_unit_sources_from_workspace_tree_v2(
            &target(CompilationUnitKeyV2::JavaModule {
                name: "fixture.module".into(),
                root: "java/src".into(),
            }),
            LanguageProfile::Java(JavaRelease::Java25),
            Stage::LowerIr,
            &tree,
        )
        .expect("Java source set candidates selected");
        assert_eq!(java.sources().len(), 2);

        let package_root_java = select_unit_sources_from_workspace_tree_v2(
            &target(CompilationUnitKeyV2::PackageRoot),
            LanguageProfile::Java(JavaRelease::Java25),
            Stage::LowerIr,
            &tree,
        )
        .expect("package-root selector follows its profile language");
        assert_eq!(package_root_java.sources().len(), 2);

        let clang = select_unit_sources_from_workspace_tree_v2(
            &target(CompilationUnitKeyV2::ClangTranslationUnit {
                is_cxx: false,
                source_path: "native/main.c".into(),
            }),
            LanguageProfile::C(CStandard::C23),
            Stage::LowerIr,
            &tree,
        )
        .expect("one exact Clang translation unit selected");
        assert_eq!(clang.sources().len(), 1);
        assert_eq!(clang.sources()[0].relative_path(), "native/main.c");

        let rust = select_unit_sources_from_workspace_tree_v2(
            &target(CompilationUnitKeyV2::RustCrate {
                name: "fixture".into(),
                root: "native/lib.rs".into(),
            }),
            LanguageProfile::Rust(backend_semantic::vocabulary::RustEdition::Rust2024),
            Stage::LowerIr,
            &tree,
        )
        .expect("Rust crate candidates selected");
        assert_eq!(rust.sources().len(), 2);
        assert_eq!(
            rust.anchor().expect("crate root").relative_path(),
            "native/lib.rs"
        );

        let csharp = select_unit_sources_from_workspace_tree_v2(
            &target(CompilationUnitKeyV2::CSharpProject {
                project_path: "dotnet/app.csproj".into(),
            }),
            LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp14),
            Stage::LowerIr,
            &tree,
        )
        .expect("C# project candidates selected");
        assert_eq!(
            csharp.anchor().expect("project anchor").relative_path(),
            "dotnet/app.csproj"
        );
        assert_eq!(csharp.sources().len(), 2);

        let changed_tree = workspace_tree(&[
            ("web/tsconfig.json", Configuration, [1; 32], 15),
            ("web/src/main.ts", Source, [99; 32], 26),
        ]);
        let changed = select_unit_sources_from_workspace_tree_v2(
            &ts_target,
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            &changed_tree,
        )
        .expect("changed source remains a valid selection");
        assert_ne!(ts.selection_digest(), changed.selection_digest());
    }

    #[test]
    fn workspace_selection_rejects_wrong_tree_profile_anchor_and_missing_source() {
        use CompilerWorkspaceFileRoleV2::{Configuration, Source};

        let key = CompilationUnitKeyV2::TypeScriptProgram {
            config_path: "web/tsconfig.json".into(),
        };
        let target = target(key.clone());
        let read_tree = CompilerInputMerkleTreeV2::from_sorted_records(
            CompilerInputTreeKindV2::ReadFrontier,
            vec![CompilerInputTreeRecordV2::AbsentPath {
                path: "web/tsconfig.json".into(),
            }],
        )
        .expect("valid read-frontier tree");
        assert_eq!(
            select_unit_sources_from_workspace_tree_v2(
                &target,
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                Stage::LowerIr,
                &read_tree,
            ),
            Err(UnitAuthorityV2Error::InvalidWorkspaceTree)
        );

        let tree = fixture_workspace_tree();
        assert_eq!(
            select_unit_sources_from_workspace_tree_v2(
                &target,
                LanguageProfile::Python(PythonVersion::Python314),
                Stage::LowerIr,
                &tree,
            ),
            Err(UnitAuthorityV2Error::ProfileKeyMismatch)
        );

        let missing_anchor = workspace_tree(&[("web/main.ts", Source, [31; 32], 12)]);
        assert_eq!(
            select_unit_sources_from_workspace_tree_v2(
                &target,
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                Stage::LowerIr,
                &missing_anchor,
            ),
            Err(UnitAuthorityV2Error::MissingUnitAnchor)
        );

        let wrong_role = workspace_tree(&[
            ("web/tsconfig.json", Source, [32; 32], 15),
            ("web/main.ts", Source, [33; 32], 12),
        ]);
        assert_eq!(
            select_unit_sources_from_workspace_tree_v2(
                &target,
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                Stage::LowerIr,
                &wrong_role,
            ),
            Err(UnitAuthorityV2Error::AnchorRoleMismatch)
        );

        let no_sources = workspace_tree(&[("web/tsconfig.json", Configuration, [34; 32], 15)]);
        assert_eq!(
            select_unit_sources_from_workspace_tree_v2(
                &target,
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                Stage::LowerIr,
                &no_sources,
            ),
            Err(UnitAuthorityV2Error::MissingLanguageSource)
        );
    }

    #[test]
    fn cross_language_remote_closure_gaps_are_typed_and_local_fallback_stays_fail_closed() {
        let cases = [
            (
                CompilationUnitKindV2::TypeScriptProgram,
                UnsupportedClosureRequirementV2::TypeScriptModuleTree,
            ),
            (
                CompilationUnitKindV2::PythonModule,
                UnsupportedClosureRequirementV2::PythonImportClosure,
            ),
            (
                CompilationUnitKindV2::GoPackage,
                UnsupportedClosureRequirementV2::GoPackageImportClosure,
            ),
            (
                CompilationUnitKindV2::JavaSourceSet,
                UnsupportedClosureRequirementV2::JavaExternalClasspath,
            ),
            (
                CompilationUnitKindV2::ClangTranslationUnit,
                UnsupportedClosureRequirementV2::ClangIncludeClosure,
            ),
        ];
        for (kind, expected) in cases {
            assert_eq!(unit_remote_closure_requirement_v2(kind), Some(expected));
        }
        assert_eq!(
            unit_remote_closure_requirement_v2(CompilationUnitKindV2::RustCrate),
            None
        );
        assert_eq!(
            unit_remote_closure_requirement_v2(CompilationUnitKindV2::CSharpProject),
            None
        );
        assert_eq!(
            unit_closure_for_language(CompilationUnitKindV2::PackageRoot, Language::TypeScript,),
            Some(UnsupportedClosureRequirementV2::TypeScriptModuleTree)
        );
        assert_eq!(
            unit_closure_for_language(CompilationUnitKindV2::PackageRoot, Language::Python,),
            Some(UnsupportedClosureRequirementV2::PythonImportClosure)
        );
        assert_eq!(
            unit_closure_for_language(CompilationUnitKindV2::PackageRoot, Language::Go),
            Some(UnsupportedClosureRequirementV2::GoPackageImportClosure)
        );
        assert_eq!(
            unit_closure_for_language(CompilationUnitKindV2::PackageRoot, Language::Java),
            Some(UnsupportedClosureRequirementV2::JavaExternalClasspath)
        );
        assert_eq!(
            unit_closure_for_language(CompilationUnitKindV2::PackageRoot, Language::Clang),
            Some(UnsupportedClosureRequirementV2::ClangIncludeClosure)
        );

        let tree = fixture_workspace_tree();
        let python_target = target(CompilationUnitKeyV2::PythonModule {
            root: "python/pkg".into(),
        });
        let profile = LanguageProfile::Python(PythonVersion::Python314);
        let selection = select_unit_sources_from_workspace_tree_v2(
            &python_target,
            profile,
            Stage::LowerIr,
            &tree,
        )
        .expect("valid Python source candidates");
        let manifest = manifest_for(
            python_target.clone(),
            profile,
            NativeTool::Python,
            tree.root(),
        );
        assert_eq!(
            manifest.read_frontier_status(),
            crate::compiler_input_manifest_v2::CompilerReadFrontierStatusV2::Unproven
        );
        assert_eq!(
            unit_execution_readiness_v2(
                &selection,
                &manifest,
                None,
                UnitFrontendAvailabilityV2::Available,
            ),
            Ok(UnitExecutionReadinessV2::LocalFallback(
                UnitExecutionFallbackReasonV2::ExecutionIdentityUnavailable,
            ))
        );

        let wrong_snapshot_manifest =
            manifest_for(python_target, profile, NativeTool::Python, [99; 32]);
        assert_eq!(
            unit_execution_readiness_v2(
                &selection,
                &wrong_snapshot_manifest,
                None,
                UnitFrontendAvailabilityV2::Available,
            ),
            Err(UnitAuthorityV2Error::SelectionBindingMismatch)
        );
    }
}
