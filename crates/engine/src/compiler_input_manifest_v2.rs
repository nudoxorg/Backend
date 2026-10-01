//! Typed full-workspace compiler input identity and target-unit binding.
//!
//! This version is intentionally conservative: it describes a complete bounded
//! workspace capture for a fresh compile. It does not assert that compiler reads
//! are complete and cannot authorize incremental reuse.

use backend_cluster_transport::ControlOffer;
use backend_execution::{
    CompilerInputIdentityClaim, CompilerInputScope, PackageLineageId, ReadManifestId,
    compiler_full_workspace_transfer_work_id,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, Language, LanguageProfile, MAX_PACKAGE_URL_BYTES, NativeTool, PackageUrl,
    Stage,
};
use backend_store::{ClosureId, ObjectId, TypedObject};
use backend_version::{
    CompilationTargetDomain, CompileRecipeDomain, ContentId, ObjectKey, RootDomain, Schema,
    SchemaIdentity, SourceFactDomain, ToolchainDomain,
};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

const TARGET_MAGIC: &[u8; 8] = b"BKCPTG02";
const MANIFEST_MAGIC: &[u8; 8] = b"BKCINP02";
const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_UNIT_KEY_BYTES: usize = 4_096;
const MAX_POLICY_IDENTITY_BYTES: usize = 512;
const INPUT_ROOT_DOMAIN: &[u8] = b"backend.compiler.full-workspace.input-root.v2\0";
const WORKSPACE_SNAPSHOT_DOMAIN: &[u8] = b"backend.compiler.full-workspace.snapshot-identity.v2\0";
const TARGET_UNIT_DOMAIN: &[u8] = b"backend.compiler.compilation-unit.v2\0";
const INVOCATION_RECIPE_MAGIC: &[u8; 8] = b"BKCRCP02";

/// Typed schema for a version-two compiler input manifest.
pub struct CompilerInputManifestV2Schema;

impl Schema for CompilerInputManifestV2Schema {
    const DOMAIN: u8 = 0xe7;
    const TYPE: u16 = 1;
    const VERSION: u8 = 2;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Portable source-independent identity of one native compiler invocation.
///
/// Host paths, local authority fingerprints, and source contents are excluded.
/// Source-specific artifact recipes are derived separately with
/// [`Self::derive_source_recipe`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerInvocationRecipeV2 {
    identity: ContentId<CompileRecipeDomain>,
    profile: LanguageProfile,
    stage: Stage,
    tool: NativeTool,
    toolchain: ContentId<ToolchainDomain>,
    environment: [u8; 32],
    target_platform: [u8; 32],
    options_digest: [u8; 32],
}

/// Identity of one captured full-workspace snapshot, not a compiler read-set proof.
///
/// The value binds the workspace tree and full-workspace input root. It says
/// nothing about positive or negative reads performed by the compiler.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CompilerWorkspaceSnapshotIdV2([u8; 32]);

impl CompilerWorkspaceSnapshotIdV2 {
    /// Returns the exact fixed-width snapshot identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Copies the exact fixed-width snapshot identity bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Compiler read-frontier completeness carried by a V2 full-workspace manifest.
///
/// V2 captures the full admitted workspace but does not instrument the compiler
/// process, so no V2 manifest can claim a complete positive/negative read set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerReadFrontierStatusV2 {
    /// Compiler reads, including environment and paths outside the captured root, are unproven.
    Unproven,
}

/// Current typed reason that a compiler read frontier is only partial.
///
/// No compiler adapter has passed review for complete positive and negative
/// read coverage. In particular, the pinned rust-analyzer path does not surface
/// module-resolution misses, project-model and loader I/O can bypass the VFS,
/// and child-process/environment/toolchain reads are not fenced into the
/// operation. Keep this separate from the status so callers that only make
/// admission decisions can continue to treat every non-complete frontier as
/// the same fail-closed state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerReadFrontierPartialReasonV2 {
    /// No registered compiler adapter accounts for every positive and negative read.
    NoRegisteredCompleteReadAdapter,
}

/// Rejection while deriving or decoding a portable invocation recipe.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CompilerInvocationRecipeV2Error {
    /// Native tool family does not match the closed language profile.
    #[error("native compiler tool does not match the language profile")]
    ToolProfileMismatch,
    /// Toolchain, environment, platform, or options identity is empty.
    #[error("compiler invocation recipe contains an empty identity")]
    EmptyIdentity,
    /// The invocation recipe wire encoding is malformed or noncanonical.
    #[error("compiler invocation recipe encoding is invalid")]
    Encoding,
}

impl CompilerInvocationRecipeV2 {
    /// Derives the only canonical portable invocation identity from explicit compiler facts.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile: LanguageProfile,
        stage: Stage,
        tool: NativeTool,
        toolchain: ContentId<ToolchainDomain>,
        environment: [u8; 32],
        target_platform: [u8; 32],
        options_digest: [u8; 32],
    ) -> Result<Self, CompilerInvocationRecipeV2Error> {
        if native_tool_for(profile.language()) != tool {
            return Err(CompilerInvocationRecipeV2Error::ToolProfileMismatch);
        }
        if *toolchain.as_ref() == [0; 32]
            || environment == [0; 32]
            || target_platform == [0; 32]
            || options_digest == [0; 32]
        {
            return Err(CompilerInvocationRecipeV2Error::EmptyIdentity);
        }
        let mut recipe = Self {
            identity: ContentId::from_digest([0; 32]),
            profile,
            stage,
            tool,
            toolchain,
            environment,
            target_platform,
            options_digest,
        };
        recipe.identity = recipe.derive_identity();
        Ok(recipe)
    }

    /// Decodes and rederives a canonical portable invocation recipe.
    pub fn decode(bytes: &[u8]) -> Result<Self, CompilerInvocationRecipeV2Error> {
        if bytes.len() != 8 + 2 + 1 + 1 + (32 * 5) || &bytes[..8] != INVOCATION_RECIPE_MAGIC {
            return Err(CompilerInvocationRecipeV2Error::Encoding);
        }
        let profile = LanguageProfile::try_from([bytes[8], bytes[9]])
            .map_err(|_| CompilerInvocationRecipeV2Error::Encoding)?;
        let stage =
            Stage::try_from(bytes[10]).map_err(|_| CompilerInvocationRecipeV2Error::Encoding)?;
        let tool = NativeTool::try_from(bytes[11])
            .map_err(|_| CompilerInvocationRecipeV2Error::Encoding)?;
        let toolchain = ContentId::<ToolchainDomain>::try_from(&bytes[12..44])
            .map_err(|_| CompilerInvocationRecipeV2Error::Encoding)?;
        let mut environment = [0; 32];
        environment.copy_from_slice(&bytes[44..76]);
        let mut target_platform = [0; 32];
        target_platform.copy_from_slice(&bytes[76..108]);
        let mut options_digest = [0; 32];
        options_digest.copy_from_slice(&bytes[108..140]);
        let observed_identity = ContentId::<CompileRecipeDomain>::try_from(&bytes[140..172])
            .map_err(|_| CompilerInvocationRecipeV2Error::Encoding)?;
        let recipe = Self::new(
            profile,
            stage,
            tool,
            toolchain,
            environment,
            target_platform,
            options_digest,
        )?;
        if recipe.identity != observed_identity || recipe.encode().as_slice() != bytes {
            return Err(CompilerInvocationRecipeV2Error::Encoding);
        }
        Ok(recipe)
    }

    /// Returns canonical recipe bytes, including its derived recipe identity.
    pub fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(172);
        bytes.extend_from_slice(INVOCATION_RECIPE_MAGIC);
        bytes.extend_from_slice(&<[u8; 2]>::from(self.profile));
        bytes.push(u8::from(self.stage));
        bytes.push(u8::from(self.tool));
        bytes.extend_from_slice(self.toolchain.as_ref());
        bytes.extend_from_slice(&self.environment);
        bytes.extend_from_slice(&self.target_platform);
        bytes.extend_from_slice(&self.options_digest);
        bytes.extend_from_slice(self.identity.as_ref());
        bytes
    }

    /// Derives the existing source-specific artifact recipe for one source identity.
    #[must_use]
    pub fn derive_source_recipe(self, source: ContentId<SourceFactDomain>) -> CompileRecipeFact {
        CompileRecipeFact::derive(self.profile, self.stage, self.tool, source, self.toolchain)
    }

    /// Portable scalar identity used by compiler work scope and offers.
    #[must_use]
    pub const fn identity(self) -> ContentId<CompileRecipeDomain> {
        self.identity
    }

    /// Closed language/profile discriminator.
    #[must_use]
    pub const fn profile(self) -> LanguageProfile {
        self.profile
    }

    /// Closed compiler stage discriminator.
    #[must_use]
    pub const fn stage(self) -> Stage {
        self.stage
    }

    /// Native compiler tool family.
    #[must_use]
    pub const fn tool(self) -> NativeTool {
        self.tool
    }

    /// Exact resolved toolchain digest.
    #[must_use]
    pub const fn toolchain(self) -> ContentId<ToolchainDomain> {
        self.toolchain
    }

    /// Exact portable compiler environment digest.
    #[must_use]
    pub const fn environment(self) -> [u8; 32] {
        self.environment
    }

    /// Exact target platform/sysroot digest.
    #[must_use]
    pub const fn target_platform(self) -> [u8; 32] {
        self.target_platform
    }

    /// Digest of portable compiler options and flags.
    #[must_use]
    pub const fn options_digest(self) -> [u8; 32] {
        self.options_digest
    }

    fn derive_identity(self) -> ContentId<CompileRecipeDomain> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.compiler.invocation-recipe.v2\0");
        hasher.update(&<[u8; 2]>::from(self.profile));
        hasher.update(&[u8::from(self.stage), u8::from(self.tool)]);
        hasher.update(self.toolchain.as_ref());
        hasher.update(&self.environment);
        hasher.update(&self.target_platform);
        hasher.update(&self.options_digest);
        ContentId::from_digest(*hasher.finalize().as_bytes())
    }
}

fn native_tool_for(language: Language) -> NativeTool {
    match language {
        Language::Rust => NativeTool::Rustc,
        Language::TypeScript => NativeTool::TypeScriptCompiler,
        Language::Python => NativeTool::Python,
        Language::Go => NativeTool::GoCompiler,
        Language::Java => NativeTool::JavaCompiler,
        Language::CSharp => NativeTool::CSharpCompiler,
        Language::Clang => NativeTool::Clang,
    }
}

/// One language-native compilation cell within a package lineage.
///
/// Paths use the same portable relative-path grammar as the compiler input
/// inventory. This prevents language authorities from collapsing native
/// targets into arbitrary language buckets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompilationUnitKeyV2 {
    /// A package-wide unit for a genuinely single-boundary authority.
    PackageRoot,
    /// A Rust crate target and its crate-root source file.
    RustCrate { name: Box<str>, root: Box<str> },
    /// A TypeScript/JavaScript program rooted at one exact configuration file.
    TypeScriptProgram { config_path: Box<str> },
    /// A Python package or module root.
    PythonModule { root: Box<str> },
    /// A Go module or package directory.
    GoPackage { root: Box<str> },
    /// A Java module name and root directory.
    JavaModule { name: Box<str>, root: Box<str> },
    /// A C# project file.
    CSharpProject { project_path: Box<str> },
    /// One C or C++ translation unit.
    ClangTranslationUnit { is_cxx: bool, source_path: Box<str> },
}

impl CompilationUnitKeyV2 {
    fn encode(&self) -> Result<Vec<u8>, CompilerPackageTargetV2Error> {
        let mut output = Vec::new();
        match self {
            Self::PackageRoot => output.push(1),
            Self::RustCrate { name, root } => {
                output.push(2);
                put_unit_text(&mut output, name, false)?;
                put_unit_path(&mut output, root)?;
            }
            Self::TypeScriptProgram { config_path } => {
                output.push(3);
                put_unit_path(&mut output, config_path)?;
            }
            Self::PythonModule { root } => {
                output.push(4);
                put_unit_path(&mut output, root)?;
            }
            Self::GoPackage { root } => {
                output.push(5);
                put_unit_path(&mut output, root)?;
            }
            Self::JavaModule { name, root } => {
                output.push(6);
                put_unit_text(&mut output, name, true)?;
                put_unit_path(&mut output, root)?;
            }
            Self::CSharpProject { project_path } => {
                output.push(7);
                put_unit_path(&mut output, project_path)?;
            }
            Self::ClangTranslationUnit {
                is_cxx,
                source_path,
            } => {
                output.push(8);
                output.push(u8::from(*is_cxx));
                put_unit_path(&mut output, source_path)?;
            }
        }
        if output.len() > MAX_UNIT_KEY_BYTES {
            return Err(CompilerPackageTargetV2Error::Limit);
        }
        Ok(output)
    }

    fn decode(bytes: &[u8]) -> Result<Self, CompilerPackageTargetV2Error> {
        if bytes.is_empty() || bytes.len() > MAX_UNIT_KEY_BYTES {
            return Err(CompilerPackageTargetV2Error::Limit);
        }
        let mut reader = Reader::new(bytes);
        let unit = match reader.byte()? {
            1 => Self::PackageRoot,
            2 => Self::RustCrate {
                name: reader.text(false)?.into_boxed_str(),
                root: reader.path()?.into_boxed_str(),
            },
            3 => Self::TypeScriptProgram {
                config_path: reader.path()?.into_boxed_str(),
            },
            4 => Self::PythonModule {
                root: reader.path()?.into_boxed_str(),
            },
            5 => Self::GoPackage {
                root: reader.path()?.into_boxed_str(),
            },
            6 => Self::JavaModule {
                name: reader.text(true)?.into_boxed_str(),
                root: reader.path()?.into_boxed_str(),
            },
            7 => Self::CSharpProject {
                project_path: reader.path()?.into_boxed_str(),
            },
            8 => Self::ClangTranslationUnit {
                is_cxx: match reader.byte()? {
                    0 => false,
                    1 => true,
                    _ => return Err(CompilerPackageTargetV2Error::CompilationUnitKey),
                },
                source_path: reader.path()?.into_boxed_str(),
            },
            _ => return Err(CompilerPackageTargetV2Error::CompilationUnitKey),
        };
        if !reader.is_empty() || unit.encode()?.as_slice() != bytes {
            return Err(CompilerPackageTargetV2Error::NonCanonical);
        }
        Ok(unit)
    }
}

/// Canonical package URL paired with one exact native compilation unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerPackageTargetV2 {
    package: PackageUrl,
    unit: CompilationUnitKeyV2,
    target: ContentId<CompilationTargetDomain>,
}

/// Invalid or noncanonical V2 package-to-target binding.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CompilerPackageTargetV2Error {
    /// The package URL is not a supported canonical pinned coordinate.
    #[error("compiler input package URL is invalid")]
    PackageUrl,
    /// The encoded target is not derived from this package and unit key.
    #[error("compiler input target does not identify its package and compilation unit")]
    PackageTargetMismatch,
    /// The language-specific compilation unit key is not canonical.
    #[error("compiler compilation-unit key is invalid")]
    CompilationUnitKey,
    /// The binding has an invalid format header.
    #[error("compiler package-target binding header is invalid")]
    Header,
    /// The encoded binding ended before all fields were complete.
    #[error("compiler package-target binding is truncated")]
    Truncated,
    /// The encoding contains trailing bytes or a noncanonical length.
    #[error("compiler package-target binding is not canonical")]
    NonCanonical,
    /// A package field exceeds its fixed wire bound.
    #[error("compiler package-target binding exceeds its fixed bound")]
    Limit,
}

impl CompilerPackageTargetV2 {
    /// Creates one exact target identity from a canonical package and typed unit key.
    pub fn new(
        package: PackageUrl,
        unit: CompilationUnitKeyV2,
    ) -> Result<Self, CompilerPackageTargetV2Error> {
        let unit_bytes = unit.encode()?;
        let target = derive_target(&package, &unit_bytes);
        Ok(Self {
            package,
            unit,
            target,
        })
    }

    /// Creates the package-wide target used only by a single-boundary authority.
    #[must_use]
    pub fn for_package(package: PackageUrl) -> Self {
        Self {
            target: package.identity,
            package,
            unit: CompilationUnitKeyV2::PackageRoot,
        }
    }

    /// Returns the exact canonical package URL.
    #[must_use]
    pub const fn package(&self) -> &PackageUrl {
        &self.package
    }

    /// Returns the exact language-native compilation unit.
    #[must_use]
    pub const fn unit_key(&self) -> &CompilationUnitKeyV2 {
        &self.unit
    }

    /// Returns the target identity derived from package plus native unit.
    #[must_use]
    pub const fn target(&self) -> ContentId<CompilationTargetDomain> {
        self.target
    }

    /// Encodes package spelling, unit key, and derived target in a bounded wire form.
    pub fn encode(&self) -> Result<Vec<u8>, CompilerPackageTargetV2Error> {
        let package = self.package.as_str().as_bytes();
        let unit = self.unit.encode()?;
        if package.is_empty() || package.len() > MAX_PACKAGE_URL_BYTES {
            return Err(CompilerPackageTargetV2Error::Limit);
        }
        if derive_target(&self.package, &unit) != self.target {
            return Err(CompilerPackageTargetV2Error::PackageTargetMismatch);
        }
        let mut bytes =
            Vec::with_capacity(TARGET_MAGIC.len() + 4 + package.len() + unit.len() + 32);
        bytes.extend_from_slice(TARGET_MAGIC);
        put_u16(&mut bytes, package.len())?;
        bytes.extend_from_slice(package);
        put_u16(&mut bytes, unit.len())?;
        bytes.extend_from_slice(&unit);
        bytes.extend_from_slice(self.target.as_ref());
        Ok(bytes)
    }

    /// Decodes and recomputes the package-plus-unit target relationship.
    pub fn decode(bytes: &[u8]) -> Result<Self, CompilerPackageTargetV2Error> {
        let mut reader = Reader::new(bytes);
        if reader.take(TARGET_MAGIC.len())? != TARGET_MAGIC {
            return Err(CompilerPackageTargetV2Error::Header);
        }
        let package_length = reader.u16()?;
        if package_length == 0 || package_length > MAX_PACKAGE_URL_BYTES {
            return Err(CompilerPackageTargetV2Error::Limit);
        }
        let package_text = std::str::from_utf8(reader.take(package_length)?)
            .map_err(|_| CompilerPackageTargetV2Error::PackageUrl)?
            .to_owned();
        let package = PackageUrl::parse(package_text)
            .map_err(|_| CompilerPackageTargetV2Error::PackageUrl)?;
        let unit_length = reader.u16()?;
        let unit = CompilationUnitKeyV2::decode(reader.take(unit_length)?)?;
        let target_bytes = reader
            .array32()
            .map_err(|_| CompilerPackageTargetV2Error::Truncated)?;
        let target = ContentId::<CompilationTargetDomain>::try_from(target_bytes)
            .map_err(|_| CompilerPackageTargetV2Error::PackageTargetMismatch)?;
        if !reader.is_empty() {
            return Err(CompilerPackageTargetV2Error::NonCanonical);
        }
        let expected = Self::new(package, unit)?;
        if expected.target != target || expected.encode()?.as_slice() != bytes {
            return Err(CompilerPackageTargetV2Error::PackageTargetMismatch);
        }
        Ok(expected)
    }
}

/// Canonical identity fields for one fresh full-workspace compile capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerInputManifestV2 {
    package_target: CompilerPackageTargetV2,
    package_lineage: [u8; 32],
    invocation_recipe: CompilerInvocationRecipeV2,
    source_provenance: [u8; 32],
    policy_identity: Box<str>,
    source_fence_digest: [u8; 32],
    workspace_root: [u8; 32],
    input_root: [u8; 32],
    workspace_snapshot_id: CompilerWorkspaceSnapshotIdV2,
    max_output_bytes: u64,
}

/// Invalid, oversized, or noncanonical version-two compiler input manifest.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CompilerInputManifestV2Error {
    /// The bytes do not have the supported manifest version header.
    #[error("compiler input manifest V2 header is invalid")]
    Header,
    /// The manifest ended before a required field was complete.
    #[error("compiler input manifest V2 is truncated")]
    Truncated,
    /// A field uses an unknown enum value or invalid canonical value.
    #[error("compiler input manifest V2 contains an invalid field")]
    InvalidField,
    /// The manifest is not in canonical encoding or has an invalid identity relation.
    #[error("compiler input manifest V2 is not canonical")]
    NonCanonical,
    /// A field or full encoded object exceeds its fixed bound.
    #[error("compiler input manifest V2 exceeds a fixed bound")]
    Limit,
    /// The supplied typed object uses another schema.
    #[error("typed object is not a compiler input manifest V2")]
    SchemaMismatch,
    /// The typed object's key does not bind its canonical bytes.
    #[error("compiler input manifest V2 object key does not match its bytes")]
    ObjectKeyMismatch,
    /// The package URL is not a supported canonical pinned coordinate.
    #[error("compiler input manifest V2 package URL is invalid")]
    PackageUrl,
    /// The package and compilation-unit target binding is invalid.
    #[error("compiler input manifest V2 target binding is invalid")]
    PackageTarget,
    /// The portable invocation recipe is invalid or disagrees with its identity.
    #[error("compiler input manifest V2 invocation recipe is invalid")]
    InvocationRecipe,
    /// The full-workspace capture or exact assignment does not match the offer.
    #[error("compiler input manifest V2 does not match the fresh full-workspace offer")]
    OfferMismatch,
    /// A wire identity digest cannot be admitted as its typed execution identity.
    #[error("compiler input manifest V2 contains an invalid typed assignment identity")]
    TypedIdentity,
}

impl From<CompilerPackageTargetV2Error> for CompilerInputManifestV2Error {
    fn from(_: CompilerPackageTargetV2Error) -> Self {
        Self::PackageTarget
    }
}

impl From<CompilerInvocationRecipeV2Error> for CompilerInputManifestV2Error {
    fn from(_: CompilerInvocationRecipeV2Error) -> Self {
        Self::InvocationRecipe
    }
}

impl CompilerInputManifestV2 {
    /// Creates one fresh full-workspace manifest with roots derived from captured facts.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        package_target: CompilerPackageTargetV2,
        package_lineage: [u8; 32],
        invocation_recipe: CompilerInvocationRecipeV2,
        source_provenance: [u8; 32],
        policy_identity: impl Into<Box<str>>,
        source_fence_digest: [u8; 32],
        workspace_root: [u8; 32],
        max_output_bytes: u64,
    ) -> Result<Self, CompilerInputManifestV2Error> {
        let policy_identity = policy_identity.into();
        let mut manifest = Self {
            package_target,
            package_lineage,
            invocation_recipe,
            source_provenance,
            policy_identity,
            source_fence_digest,
            workspace_root,
            input_root: [0; 32],
            workspace_snapshot_id: CompilerWorkspaceSnapshotIdV2([0; 32]),
            max_output_bytes,
        };
        manifest.input_root = manifest.derive_input_root()?;
        manifest.workspace_snapshot_id = manifest.derive_workspace_snapshot_id();
        manifest.validate()?;
        Ok(manifest)
    }

    /// Returns canonical version-two bytes.
    pub fn encode(&self) -> Result<Vec<u8>, CompilerInputManifestV2Error> {
        self.validate()?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MANIFEST_MAGIC);
        put_blob(&mut bytes, &self.package_target.encode()?)?;
        bytes.extend_from_slice(&self.package_lineage);
        put_blob(&mut bytes, &self.invocation_recipe.encode())?;
        bytes.extend_from_slice(&self.source_provenance);
        put_blob(&mut bytes, self.policy_identity.as_bytes())?;
        bytes.extend_from_slice(&self.source_fence_digest);
        bytes.extend_from_slice(&self.workspace_root);
        bytes.extend_from_slice(&self.input_root);
        bytes.extend_from_slice(self.workspace_snapshot_id.as_bytes());
        bytes.extend_from_slice(&self.max_output_bytes.to_be_bytes());
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(CompilerInputManifestV2Error::Limit);
        }
        Ok(bytes)
    }

    /// Decodes a canonical fresh full-workspace manifest and rederives its input identity.
    pub fn decode(bytes: &[u8]) -> Result<Self, CompilerInputManifestV2Error> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(CompilerInputManifestV2Error::Limit);
        }
        let mut reader = Reader::new(bytes);
        if reader.take(MANIFEST_MAGIC.len()).map_err(map_read_error)? != MANIFEST_MAGIC {
            return Err(CompilerInputManifestV2Error::Header);
        }
        let package_target = CompilerPackageTargetV2::decode(
            reader.blob(MAX_UNIT_KEY_BYTES + MAX_PACKAGE_URL_BYTES + 64)?,
        )?;
        let package_lineage = reader.array32()?;
        let invocation_recipe = CompilerInvocationRecipeV2::decode(reader.blob(256)?)?;
        let source_provenance = reader.array32()?;
        let policy_identity = reader
            .text_with_limit(MAX_POLICY_IDENTITY_BYTES)?
            .into_boxed_str();
        let source_fence_digest = reader.array32()?;
        let workspace_root = reader.array32()?;
        let input_root = reader.array32()?;
        let workspace_snapshot_id = CompilerWorkspaceSnapshotIdV2(reader.array32()?);
        let max_output_bytes = reader.u64()?;
        if !reader.is_empty() {
            return Err(CompilerInputManifestV2Error::NonCanonical);
        }
        let manifest = Self {
            package_target,
            package_lineage,
            invocation_recipe,
            source_provenance,
            policy_identity,
            source_fence_digest,
            workspace_root,
            input_root,
            workspace_snapshot_id,
            max_output_bytes,
        };
        manifest.validate()?;
        if manifest.encode()?.as_slice() != bytes {
            return Err(CompilerInputManifestV2Error::NonCanonical);
        }
        Ok(manifest)
    }

    /// Reopens a stored typed object and verifies schema, key, and canonical bytes.
    pub fn from_typed_object(object: &TypedObject) -> Result<Self, CompilerInputManifestV2Error> {
        let schema = object.schema();
        if schema.domain() != CompilerInputManifestV2Schema::DOMAIN
            || schema.ty() != CompilerInputManifestV2Schema::TYPE
            || schema.version() != CompilerInputManifestV2Schema::VERSION
        {
            return Err(CompilerInputManifestV2Error::SchemaMismatch);
        }
        let manifest = Self::decode(object.bytes())?;
        let key = ObjectKey::<CompilerInputManifestV2Schema>::from_value(object.bytes());
        if object.key() != &key.to_bytes() {
            return Err(CompilerInputManifestV2Error::ObjectKeyMismatch);
        }
        Ok(manifest)
    }

    /// Encodes this manifest as one typed immutable backend-store object.
    pub fn typed_object(&self) -> Result<TypedObject, CompilerInputManifestV2Error> {
        let bytes = self.encode()?;
        let key = ObjectKey::<CompilerInputManifestV2Schema>::from_value(bytes.as_slice());
        Ok(TypedObject::from_value(&key, bytes.as_slice()))
    }

    /// Returns the exact typed manifest ObjectId included in a capture closure.
    pub fn object_id(&self) -> Result<ObjectId, CompilerInputManifestV2Error> {
        Ok(self.typed_object()?.id())
    }

    /// Checks exact fresh full-workspace assignment fields and recomputes the V2 transfer work ID.
    ///
    /// The workspace snapshot identity in the offer does not claim complete compiler reads.
    pub fn matches_offer(
        &self,
        offer: &ControlOffer,
        checked_input_closure: ClosureId,
    ) -> Result<bool, CompilerInputManifestV2Error> {
        self.validate()?;
        if offer.selected_base.is_some()
            || offer.input_closure_id != *checked_input_closure.as_bytes()
            || offer.input_manifest_object_id != *self.object_id()?.as_bytes()
            || offer.package_lineage != self.package_lineage
            || offer.target != *self.package_target.target().as_ref()
            || offer.recipe != self.recipe()
            || offer.input_root != self.input_root
            || offer.read_manifest != self.workspace_snapshot_id.to_bytes()
            || offer.max_output_bytes != self.max_output_bytes
        {
            return Ok(false);
        }
        let work_id = compiler_full_workspace_transfer_work_id(
            self.package_lineage,
            *self.package_target.target().as_ref(),
            self.recipe(),
            self.workspace_snapshot_id.to_bytes(),
            self.input_root,
            *checked_input_closure.as_bytes(),
            *self.object_id()?.as_bytes(),
            self.max_output_bytes,
        );
        Ok(offer.scope.work_id == work_id)
    }

    /// Admits a typed object for one offer after the caller has checked the whole closure.
    pub fn admit_for_offer(
        object: &TypedObject,
        offer: &ControlOffer,
        checked_input_closure: ClosureId,
    ) -> Result<Self, CompilerInputManifestV2Error> {
        let manifest = Self::from_typed_object(object)?;
        if !manifest.matches_offer(offer, checked_input_closure)? {
            return Err(CompilerInputManifestV2Error::OfferMismatch);
        }
        Ok(manifest)
    }

    /// Package and unit binding.
    #[must_use]
    pub const fn package_target(&self) -> &CompilerPackageTargetV2 {
        &self.package_target
    }

    /// Source-aware package lineage digest.
    #[must_use]
    pub const fn package_lineage(&self) -> [u8; 32] {
        self.package_lineage
    }

    /// Closed language/profile discriminator.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.invocation_recipe.profile()
    }

    /// Closed compiler stage discriminator.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.invocation_recipe.stage()
    }

    /// Exact compile recipe identity.
    #[must_use]
    pub fn recipe(&self) -> [u8; 32] {
        *self.invocation_recipe.identity().as_ref()
    }

    /// Exact executable/toolchain identity.
    #[must_use]
    pub fn toolchain(&self) -> [u8; 32] {
        *self.invocation_recipe.toolchain().as_ref()
    }

    /// Exact compiler environment identity.
    #[must_use]
    pub const fn environment(&self) -> [u8; 32] {
        self.invocation_recipe.environment()
    }

    /// Exact target platform/sysroot identity.
    #[must_use]
    pub const fn target_platform(&self) -> [u8; 32] {
        self.invocation_recipe.target_platform()
    }

    /// Source/archive provenance digest.
    #[must_use]
    pub const fn source_provenance(&self) -> [u8; 32] {
        self.source_provenance
    }

    /// Exact captured ignore/generated-files policy bytes.
    #[must_use]
    pub fn policy_identity(&self) -> &str {
        &self.policy_identity
    }

    /// Pairing token for the exact scanner revision fence; revalidation remains mandatory.
    #[must_use]
    pub const fn source_fence_digest(&self) -> [u8; 32] {
        self.source_fence_digest
    }

    /// Root page ObjectId of the full ordered workspace inventory.
    #[must_use]
    pub const fn workspace_root(&self) -> [u8; 32] {
        self.workspace_root
    }

    /// Input identity derived from workspace content and typed invocation facts.
    #[must_use]
    pub const fn input_root(&self) -> [u8; 32] {
        self.input_root
    }

    /// Typed identity of the captured workspace tree.
    #[must_use]
    pub const fn workspace_snapshot_id(&self) -> CompilerWorkspaceSnapshotIdV2 {
        self.workspace_snapshot_id
    }

    /// Read-frontier completeness; full-workspace capture alone is always unproven.
    #[must_use]
    pub const fn read_frontier_status(&self) -> CompilerReadFrontierStatusV2 {
        CompilerReadFrontierStatusV2::Unproven
    }

    /// Typed reason no compiler authority can currently provide a complete frontier.
    #[must_use]
    pub const fn read_frontier_partial_reason(&self) -> CompilerReadFrontierPartialReasonV2 {
        CompilerReadFrontierPartialReasonV2::NoRegisteredCompleteReadAdapter
    }

    /// Compatibility bytes carried in the existing assignment `read_manifest` field.
    ///
    /// These bytes identify the workspace snapshot and do not prove completeness of
    /// compiler positive/negative reads. V2 work remains fresh-only until a compiler
    /// authority admits a separate read-frontier proof.
    #[must_use]
    pub const fn read_manifest(&self) -> [u8; 32] {
        self.workspace_snapshot_id.to_bytes()
    }

    /// Full workspace captures are fresh-only and have no selected base.
    #[must_use]
    pub const fn selected_base(&self) -> Option<[u8; 32]> {
        None
    }

    /// Maximum output bytes accepted for this compile.
    #[must_use]
    pub const fn max_output_bytes(&self) -> u64 {
        self.max_output_bytes
    }

    /// Typed portable source-independent compiler invocation recipe.
    #[must_use]
    pub const fn invocation_recipe(&self) -> CompilerInvocationRecipeV2 {
        self.invocation_recipe
    }

    /// Returns the execution assignment identity for a fresh full-workspace compile.
    ///
    /// The `ReadManifestId` field is a compatibility projection of the workspace snapshot
    /// identity. This method does not produce complete compiler-read evidence.
    pub fn identity_claim(
        &self,
    ) -> Result<CompilerInputIdentityClaim, CompilerInputManifestV2Error> {
        self.validate()?;
        let package = PackageLineageId::from_bytes(self.package_lineage)
            .map_err(|_| CompilerInputManifestV2Error::TypedIdentity)?;
        let input_root = ContentId::<RootDomain>::try_from(self.input_root.as_slice())
            .map_err(|_| CompilerInputManifestV2Error::TypedIdentity)?;
        let read_manifest = self.derive_workspace_assignment_id();
        Ok(CompilerInputIdentityClaim {
            scope: CompilerInputScope {
                package,
                target: self.package_target.target(),
                recipe: self.invocation_recipe.identity(),
            },
            input_root,
            manifest: read_manifest,
        })
    }

    fn validate(&self) -> Result<(), CompilerInputManifestV2Error> {
        let policy = self.policy_identity.as_bytes();
        if self.package_target.encode().is_err()
            || self.package_lineage == [0; 32]
            || self.source_provenance == [0; 32]
            || self.source_fence_digest == [0; 32]
            || self.workspace_root == [0; 32]
            || policy.is_empty()
            || policy.len() > MAX_POLICY_IDENTITY_BYTES
            || !policy.is_ascii()
            || self.max_output_bytes == 0
        {
            return Err(CompilerInputManifestV2Error::InvalidField);
        }
        if self.derive_input_root()? != self.input_root
            || self.derive_workspace_snapshot_id() != self.workspace_snapshot_id
        {
            return Err(CompilerInputManifestV2Error::NonCanonical);
        }
        Ok(())
    }

    fn derive_input_root(&self) -> Result<[u8; 32], CompilerInputManifestV2Error> {
        let policy_length = u32::try_from(self.policy_identity.len())
            .map_err(|_| CompilerInputManifestV2Error::Limit)?;
        let target = self.package_target.encode()?;
        let mut canonical = Vec::with_capacity(
            INPUT_ROOT_DOMAIN.len()
                + 32
                + 4
                + self.policy_identity.len()
                + 32
                + 32
                + target.len()
                + 32
                + 32,
        );
        canonical.extend_from_slice(INPUT_ROOT_DOMAIN);
        canonical.extend_from_slice(&self.workspace_root);
        canonical.extend_from_slice(&policy_length.to_be_bytes());
        canonical.extend_from_slice(self.policy_identity.as_bytes());
        canonical.extend_from_slice(&self.source_provenance);
        canonical.extend_from_slice(&self.source_fence_digest);
        canonical.extend_from_slice(&target);
        canonical.extend_from_slice(&self.package_lineage);
        canonical.extend_from_slice(self.invocation_recipe.identity().as_ref());
        Ok(*ContentId::<RootDomain>::from_canonical_bytes(&canonical).as_ref())
    }

    fn derive_workspace_snapshot_id(&self) -> CompilerWorkspaceSnapshotIdV2 {
        CompilerWorkspaceSnapshotIdV2(*self.derive_workspace_assignment_id().as_bytes())
    }

    fn derive_workspace_assignment_id(&self) -> ReadManifestId {
        let mut canonical = Vec::with_capacity(WORKSPACE_SNAPSHOT_DOMAIN.len() + 64);
        canonical.extend_from_slice(WORKSPACE_SNAPSHOT_DOMAIN);
        canonical.extend_from_slice(&self.workspace_root);
        canonical.extend_from_slice(&self.input_root);
        ReadManifestId::from_value(canonical.as_slice())
    }
}

fn derive_target(package: &PackageUrl, unit_bytes: &[u8]) -> ContentId<CompilationTargetDomain> {
    if matches!(unit_bytes, [1]) {
        return package.identity;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(TARGET_UNIT_DOMAIN);
    hasher.update(package.identity.as_ref());
    hasher.update(&(unit_bytes.len() as u32).to_be_bytes());
    hasher.update(unit_bytes);
    ContentId::from_digest(*hasher.finalize().as_bytes())
}

/// Validates the scanner's UTF-8 NFC, portable slash-separated path grammar.
pub(super) fn validate_compiler_input_path_v2(
    path: &str,
) -> Result<(), CompilerPackageTargetV2Error> {
    if path.is_empty()
        || path.len() > 4_096
        || path.contains('\\')
        || path.contains('\0')
        || path.starts_with('/')
        || path.nfc().collect::<String>() != path
        || path.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with('.')
                || part.ends_with(' ')
                || part.chars().any(|character| {
                    character.is_control()
                        || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
                })
                || is_windows_device_name_v2(part)
        })
    {
        return Err(CompilerPackageTargetV2Error::CompilationUnitKey);
    }
    Ok(())
}

fn is_windows_device_name_v2(component: &str) -> bool {
    let basename = component
        .split_once('.')
        .map_or(component, |(basename, _)| basename)
        .to_uppercase();
    matches!(basename.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            basename.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 1
                    && suffix
                        .as_bytes()
                        .first()
                        .is_some_and(|digit| (b'1'..=b'9').contains(digit))
            })
        })
}

fn put_unit_path(output: &mut Vec<u8>, path: &str) -> Result<(), CompilerPackageTargetV2Error> {
    validate_compiler_input_path_v2(path)
        .map_err(|_| CompilerPackageTargetV2Error::CompilationUnitKey)?;
    put_u16(output, path.len())?;
    output.extend_from_slice(path.as_bytes());
    Ok(())
}

fn put_unit_text(
    output: &mut Vec<u8>,
    text: &str,
    dotted: bool,
) -> Result<(), CompilerPackageTargetV2Error> {
    if text.is_empty()
        || text.len() > MAX_UNIT_KEY_BYTES
        || !text.is_ascii()
        || text.bytes().any(|byte| {
            byte.is_ascii_control()
                || if dotted {
                    !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
                } else {
                    !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                }
        })
        || (dotted && (text.starts_with('.') || text.ends_with('.') || text.contains("..")))
    {
        return Err(CompilerPackageTargetV2Error::CompilationUnitKey);
    }
    put_u16(output, text.len())?;
    output.extend_from_slice(text.as_bytes());
    Ok(())
}

fn put_u16(output: &mut Vec<u8>, length: usize) -> Result<(), CompilerPackageTargetV2Error> {
    let length = u16::try_from(length).map_err(|_| CompilerPackageTargetV2Error::Limit)?;
    output.extend_from_slice(&length.to_be_bytes());
    Ok(())
}

fn put_blob(output: &mut Vec<u8>, blob: &[u8]) -> Result<(), CompilerInputManifestV2Error> {
    let length = u32::try_from(blob.len()).map_err(|_| CompilerInputManifestV2Error::Limit)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(blob);
    Ok(())
}

struct Reader<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> Reader<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'bytes [u8], CompilerPackageTargetV2Error> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(CompilerPackageTargetV2Error::Limit)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(CompilerPackageTargetV2Error::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, CompilerPackageTargetV2Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<usize, CompilerPackageTargetV2Error> {
        let bytes = self.take(2)?;
        Ok(usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
    }

    fn array2(&mut self) -> Result<[u8; 2], CompilerInputManifestV2Error> {
        self.take(2)
            .map_err(map_read_error)?
            .try_into()
            .map_err(|_| CompilerInputManifestV2Error::Truncated)
    }

    fn array32(&mut self) -> Result<[u8; 32], CompilerInputManifestV2Error> {
        self.take(32)
            .map_err(map_read_error)?
            .try_into()
            .map_err(|_| CompilerInputManifestV2Error::Truncated)
    }

    fn u64(&mut self) -> Result<u64, CompilerInputManifestV2Error> {
        let bytes = self.take(8).map_err(map_read_error)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn blob(&mut self, max: usize) -> Result<&'bytes [u8], CompilerInputManifestV2Error> {
        let length = self
            .u32()
            .map_err(|_| CompilerInputManifestV2Error::Truncated)?;
        if length > max {
            return Err(CompilerInputManifestV2Error::Limit);
        }
        self.take(length).map_err(map_read_error)
    }

    fn u32(&mut self) -> Result<usize, CompilerInputManifestV2Error> {
        let bytes = self.take(4).map_err(map_read_error)?;
        let value = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        usize::try_from(value).map_err(|_| CompilerInputManifestV2Error::Limit)
    }

    fn text_with_limit(&mut self, max: usize) -> Result<String, CompilerInputManifestV2Error> {
        let bytes = self.blob(max)?;
        let text =
            std::str::from_utf8(bytes).map_err(|_| CompilerInputManifestV2Error::InvalidField)?;
        Ok(text.to_owned())
    }

    fn text(&mut self, dotted: bool) -> Result<String, CompilerPackageTargetV2Error> {
        let length = self.u16()?;
        let bytes = self.take(length)?;
        let text = std::str::from_utf8(bytes)
            .map_err(|_| CompilerPackageTargetV2Error::CompilationUnitKey)?;
        let mut validation = Vec::new();
        put_unit_text(&mut validation, text, dotted)?;
        Ok(text.to_owned())
    }

    fn path(&mut self) -> Result<String, CompilerPackageTargetV2Error> {
        let length = self.u16()?;
        if length == 0 || length > MAX_UNIT_KEY_BYTES {
            return Err(CompilerPackageTargetV2Error::Limit);
        }
        let text = std::str::from_utf8(self.take(length)?)
            .map_err(|_| CompilerPackageTargetV2Error::CompilationUnitKey)?
            .to_owned();
        validate_compiler_input_path_v2(&text)
            .map_err(|_| CompilerPackageTargetV2Error::CompilationUnitKey)?;
        Ok(text)
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn map_read_error(_: CompilerPackageTargetV2Error) -> CompilerInputManifestV2Error {
    CompilerInputManifestV2Error::Truncated
}

/// Returns the schema identity needed by the streaming CAS capture API.
#[must_use]
pub const fn compiler_input_manifest_v2_schema() -> SchemaIdentity {
    SchemaIdentity::new(
        CompilerInputManifestV2Schema::DOMAIN,
        CompilerInputManifestV2Schema::TYPE,
        CompilerInputManifestV2Schema::VERSION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler_input_tree_v2::{
        CompilerInputMerkleTreeV2, CompilerInputTreeKindV2, CompilerInputTreeRecordV2,
        CompilerWorkspaceFileRoleV2,
    };
    use backend_cluster_transport::AssignmentScope;

    fn package(value: &str) -> PackageUrl {
        PackageUrl::parse(value.to_owned()).expect("valid package URL")
    }

    fn unit(name: &str) -> CompilationUnitKeyV2 {
        CompilationUnitKeyV2::RustCrate {
            name: name.into(),
            root: format!("src/{name}.rs").into_boxed_str(),
        }
    }

    fn invocation(
        toolchain: &[u8],
        environment: [u8; 32],
        options: [u8; 32],
    ) -> CompilerInvocationRecipeV2 {
        CompilerInvocationRecipeV2::new(
            LanguageProfile::Rust(backend_semantic::vocabulary::RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            ContentId::<ToolchainDomain>::from_canonical_bytes(toolchain),
            environment,
            [5; 32],
            options,
        )
        .expect("valid invocation recipe")
    }

    fn manifest_for_workspace(
        workspace_root: [u8; 32],
        invocation_recipe: CompilerInvocationRecipeV2,
    ) -> CompilerInputManifestV2 {
        CompilerInputManifestV2::new(
            CompilerPackageTargetV2::new(package("pkg:cargo/workspace@1.0.0"), unit("lib"))
                .expect("typed target"),
            [1; 32],
            invocation_recipe,
            [6; 32],
            "nudox.compiler-workspace.v1/test;symlink=reject",
            [7; 32],
            workspace_root,
            64 * 1024 * 1024,
        )
        .expect("valid manifest")
    }

    fn workspace_root_with_missing_import(created: bool) -> [u8; 32] {
        let mut records = vec![
            CompilerInputTreeRecordV2::Directory { path: "".into() },
            CompilerInputTreeRecordV2::Directory { path: "src".into() },
            CompilerInputTreeRecordV2::File {
                path: "src/lib.rs".into(),
                role: CompilerWorkspaceFileRoleV2::Source,
                object_id: [0x11; 32],
                length: 24,
            },
        ];
        if created {
            records.push(CompilerInputTreeRecordV2::File {
                path: "src/optional.rs".into(),
                role: CompilerWorkspaceFileRoleV2::Source,
                object_id: [0x22; 32],
                length: 37,
            });
        }
        records.sort_by(|left, right| left.path().as_bytes().cmp(right.path().as_bytes()));
        CompilerInputMerkleTreeV2::from_sorted_records(CompilerInputTreeKindV2::Workspace, records)
            .expect("workspace tree")
            .root()
    }

    fn manifest() -> CompilerInputManifestV2 {
        manifest_for_workspace([8; 32], invocation(b"test-rustc", [4; 32], [9; 32]))
    }

    fn offer(
        manifest: &CompilerInputManifestV2,
        closure: ClosureId,
        object: ObjectId,
    ) -> ControlOffer {
        let work_id = compiler_full_workspace_transfer_work_id(
            manifest.package_lineage,
            *manifest.package_target.target().as_ref(),
            manifest.recipe(),
            manifest.workspace_snapshot_id.to_bytes(),
            manifest.input_root,
            *closure.as_bytes(),
            *object.as_bytes(),
            manifest.max_output_bytes,
        );
        ControlOffer {
            scope: AssignmentScope::new([1; 16], work_id, 1, [2; 32]).expect("scope"),
            package_lineage: manifest.package_lineage,
            target: *manifest.package_target.target().as_ref(),
            recipe: manifest.recipe(),
            input_root: manifest.input_root,
            read_manifest: manifest.workspace_snapshot_id.to_bytes(),
            input_closure_id: *closure.as_bytes(),
            input_manifest_object_id: *object.as_bytes(),
            selected_base: None,
            max_output_bytes: manifest.max_output_bytes,
            deadline_unix_ms: 100,
            input_grant_pages: 1,
        }
    }

    #[test]
    fn target_unit_keys_preserve_two_cells_in_one_package_and_reject_package_swap() {
        let workspace_package = package("pkg:cargo/workspace@1.0.0");
        let lib =
            CompilerPackageTargetV2::new(workspace_package.clone(), unit("lib")).expect("lib");
        let bin = CompilerPackageTargetV2::new(workspace_package, unit("cli")).expect("bin");
        assert_ne!(lib.target(), bin.target());
        assert_eq!(
            CompilerPackageTargetV2::decode(&lib.encode().expect("encode")),
            Ok(lib.clone())
        );

        let other_package = package("pkg:cargo/other@1.0.0");
        let forged = CompilerPackageTargetV2 {
            package: other_package,
            unit: lib.unit,
            target: lib.target,
        };
        assert_eq!(
            forged.encode(),
            Err(CompilerPackageTargetV2Error::PackageTargetMismatch)
        );
    }

    #[test]
    fn package_target_wire_preserves_typed_target_identity() {
        let package = package("pkg:cargo/workspace@1.0.0");
        let unit = CompilationUnitKeyV2::RustCrate {
            name: "lib".into(),
            root: "src/lib.rs".into(),
        };

        // Build the target key and wire bytes independently from the production encoders. This
        // keeps the domain byte in the serialized ContentId and catches treating wire bytes as
        // an untyped BLAKE3 digest during decode.
        let mut unit_wire = vec![2];
        unit_wire.extend_from_slice(&3_u16.to_be_bytes());
        unit_wire.extend_from_slice(b"lib");
        unit_wire.extend_from_slice(&10_u16.to_be_bytes());
        unit_wire.extend_from_slice(b"src/lib.rs");
        let mut target_hasher = blake3::Hasher::new();
        target_hasher.update(b"backend.compiler.compilation-unit.v2\0");
        target_hasher.update(package.identity.as_ref());
        target_hasher.update(
            &u32::try_from(unit_wire.len())
                .expect("small unit length")
                .to_be_bytes(),
        );
        target_hasher.update(&unit_wire);
        let target =
            ContentId::<CompilationTargetDomain>::from_digest(*target_hasher.finalize().as_bytes());
        let mut expected_wire = b"BKCPTG02".to_vec();
        expected_wire.extend_from_slice(
            &u16::try_from(package.as_str().len())
                .expect("small package URL")
                .to_be_bytes(),
        );
        expected_wire.extend_from_slice(package.as_str().as_bytes());
        expected_wire.extend_from_slice(
            &u16::try_from(unit_wire.len())
                .expect("small unit encoding")
                .to_be_bytes(),
        );
        expected_wire.extend_from_slice(&unit_wire);
        expected_wire.extend_from_slice(target.as_ref());

        let expected = CompilerPackageTargetV2 {
            package: package.clone(),
            unit: unit.clone(),
            target,
        };
        assert_eq!(expected.encode(), Ok(expected_wire.clone()));
        assert_eq!(
            CompilerPackageTargetV2::decode(&expected_wire),
            Ok(expected)
        );
    }

    #[test]
    fn every_native_unit_key_roundtrips_nested_portable_paths() {
        let package = package("pkg:cargo/workspace@1.0.0");
        let units = [
            CompilationUnitKeyV2::PackageRoot,
            CompilationUnitKeyV2::RustCrate {
                name: "lib_core".into(),
                root: "crates/core/src/lib.rs".into(),
            },
            CompilationUnitKeyV2::TypeScriptProgram {
                config_path: "apps/web/tsconfig.json".into(),
            },
            CompilationUnitKeyV2::PythonModule {
                root: "python/pkg/module".into(),
            },
            CompilationUnitKeyV2::GoPackage {
                root: "cmd/server/internal/http".into(),
            },
            CompilationUnitKeyV2::JavaModule {
                name: "com.example.api".into(),
                root: "modules/api/src".into(),
            },
            CompilationUnitKeyV2::CSharpProject {
                project_path: "src/Service/Service.csproj".into(),
            },
            CompilationUnitKeyV2::ClangTranslationUnit {
                is_cxx: true,
                source_path: "native/src/parser.cc".into(),
            },
        ];
        for unit in units {
            let target = CompilerPackageTargetV2::new(package.clone(), unit).expect("native unit");
            assert_eq!(
                CompilerPackageTargetV2::decode(&target.encode().expect("target encoding")),
                Ok(target)
            );
        }
        assert!(
            CompilerPackageTargetV2::new(
                package.clone(),
                CompilationUnitKeyV2::RustCrate {
                    name: "unicode".into(),
                    root: "src/é.rs".into(),
                }
            )
            .is_ok()
        );
        assert_eq!(
            CompilerPackageTargetV2::new(
                package,
                CompilationUnitKeyV2::RustCrate {
                    name: "unicode".into(),
                    root: "src/e\u{301}.rs".into(),
                }
            ),
            Err(CompilerPackageTargetV2Error::CompilationUnitKey)
        );
    }

    #[test]
    fn manifest_roundtrips_as_typed_v2_object_and_binds_derived_roots() {
        let manifest = manifest();
        let bytes = manifest.encode().expect("encode");
        assert_eq!(&bytes[..8], b"BKCINP02");
        let package_target = manifest.package_target().encode().expect("target bytes");
        let package_length = u32::from_be_bytes(bytes[8..12].try_into().expect("length"));
        assert_eq!(package_length as usize, package_target.len());
        assert_eq!(&bytes[12..12 + package_target.len()], package_target);
        let recipe_length_offset = 12 + package_length as usize + 32;
        let recipe_length = u32::from_be_bytes(
            bytes[recipe_length_offset..recipe_length_offset + 4]
                .try_into()
                .expect("recipe length"),
        ) as usize;
        let recipe_offset = recipe_length_offset + 4;
        let recipe_wire = manifest.invocation_recipe().encode();
        assert_eq!(recipe_length, recipe_wire.len());
        assert_eq!(&bytes[recipe_offset..recipe_offset + 8], b"BKCRCP02");
        assert_eq!(&bytes[recipe_offset + 8..recipe_offset + 12], &[0, 3, 1, 0]);
        assert_eq!(
            &bytes[recipe_offset..recipe_offset + recipe_length],
            recipe_wire
        );
        assert_eq!(
            CompilerInputManifestV2::decode(&bytes),
            Ok(manifest.clone())
        );
        let object = manifest.typed_object().expect("typed object");
        assert_eq!(
            CompilerInputManifestV2::from_typed_object(&object),
            Ok(manifest)
        );

        let mut forged = bytes;
        let offset = forged.len() - 8 - 32 - 32 - 32;
        forged[offset] ^= 1;
        assert!(CompilerInputManifestV2::decode(&forged).is_err());
    }

    #[test]
    fn execution_identity_adapter_binds_workspace_snapshot_without_read_proof() {
        let manifest = manifest();
        let identity = manifest.identity_claim().expect("typed identity claim");
        assert_eq!(identity.scope.target, manifest.package_target().target());
        assert_eq!(
            identity.scope.recipe,
            manifest.invocation_recipe().identity()
        );
        assert_eq!(*identity.input_root.as_ref(), manifest.input_root());
        assert_eq!(
            *identity.manifest.as_bytes(),
            manifest.workspace_snapshot_id().to_bytes()
        );
        assert_eq!(
            manifest.workspace_snapshot_id().as_bytes(),
            &manifest.read_manifest()
        );
        assert_eq!(
            manifest.read_frontier_status(),
            CompilerReadFrontierStatusV2::Unproven
        );
        assert_eq!(
            manifest.read_frontier_partial_reason(),
            CompilerReadFrontierPartialReasonV2::NoRegisteredCompleteReadAdapter
        );
    }

    #[test]
    fn newly_created_missing_import_changes_workspace_identity_but_not_read_proof_status() {
        let recipe = invocation(b"test-rustc", [4; 32], [9; 32]);
        let before = manifest_for_workspace(workspace_root_with_missing_import(false), recipe);
        let after = manifest_for_workspace(workspace_root_with_missing_import(true), recipe);

        assert_ne!(before.workspace_root(), after.workspace_root());
        assert_ne!(before.input_root(), after.input_root());
        assert_ne!(
            before.workspace_snapshot_id(),
            after.workspace_snapshot_id()
        );
        assert_eq!(
            before.read_frontier_status(),
            CompilerReadFrontierStatusV2::Unproven
        );
        assert_eq!(
            after.read_frontier_status(),
            CompilerReadFrontierStatusV2::Unproven
        );
    }

    #[test]
    fn environment_toolchain_and_feature_changes_change_identity_without_claiming_reads() {
        let workspace_root = workspace_root_with_missing_import(true);
        let baseline =
            manifest_for_workspace(workspace_root, invocation(b"test-rustc", [4; 32], [9; 32]));
        let changed_environment = manifest_for_workspace(
            workspace_root,
            invocation(b"test-rustc", [0x44; 32], [9; 32]),
        );
        let changed_toolchain = manifest_for_workspace(
            workspace_root,
            invocation(b"test-rustc-next", [4; 32], [9; 32]),
        );
        // The options digest includes portable feature flags and invocation options.
        let changed_features = manifest_for_workspace(
            workspace_root,
            invocation(b"test-rustc", [4; 32], [0x99; 32]),
        );

        for changed in [changed_environment, changed_toolchain, changed_features] {
            assert_eq!(changed.workspace_root(), baseline.workspace_root());
            assert_ne!(changed.recipe(), baseline.recipe());
            assert_ne!(
                changed.workspace_snapshot_id(),
                baseline.workspace_snapshot_id()
            );
            assert_eq!(
                changed.read_frontier_status(),
                CompilerReadFrontierStatusV2::Unproven
            );
        }
    }

    #[test]
    fn unsampled_external_include_cannot_upgrade_workspace_identity_to_read_proof() {
        let workspace_root = workspace_root_with_missing_import(true);
        let external_include_before = b"external-header-v1";
        let external_include_after = b"external-header-v2";
        assert_ne!(external_include_before, external_include_after);

        // External paths are not a field of a full workspace capture. Identical captured
        // workspace facts therefore retain the same identity while read completeness stays
        // explicitly unproven; an instrumented compiler authority must add this read frontier.
        let before =
            manifest_for_workspace(workspace_root, invocation(b"test-rustc", [4; 32], [9; 32]));
        let after =
            manifest_for_workspace(workspace_root, invocation(b"test-rustc", [4; 32], [9; 32]));
        assert_eq!(
            before.workspace_snapshot_id(),
            after.workspace_snapshot_id()
        );
        assert_eq!(
            before.read_frontier_status(),
            CompilerReadFrontierStatusV2::Unproven
        );
        assert_eq!(
            after.read_frontier_status(),
            CompilerReadFrontierStatusV2::Unproven
        );
    }

    #[test]
    fn invocation_recipe_is_portable_and_source_recipes_remain_source_specific() {
        let invocation = manifest().invocation_recipe();
        let source_a = ContentId::<SourceFactDomain>::from_canonical_bytes(b"source-a");
        let source_b = ContentId::<SourceFactDomain>::from_canonical_bytes(b"source-b");
        let recipe_a = invocation.derive_source_recipe(source_a);
        let recipe_b = invocation.derive_source_recipe(source_b);
        assert_ne!(recipe_a.identity, recipe_b.identity);

        assert_eq!(
            CompilerInvocationRecipeV2::new(
                invocation.profile(),
                invocation.stage(),
                NativeTool::Python,
                invocation.toolchain(),
                invocation.environment(),
                invocation.target_platform(),
                invocation.options_digest(),
            ),
            Err(CompilerInvocationRecipeV2Error::ToolProfileMismatch)
        );
        let changed_options = CompilerInvocationRecipeV2::new(
            invocation.profile(),
            invocation.stage(),
            invocation.tool(),
            invocation.toolchain(),
            invocation.environment(),
            invocation.target_platform(),
            [10; 32],
        )
        .expect("changed options");
        assert_ne!(invocation.identity(), changed_options.identity());

        // Local tool paths and authority fingerprints deliberately do not enter
        // the portable constructor, so equivalent invocations across hosts stay
        // identical even when their local installations differ.
        let host_a_path = "/opt/toolchains/rustc";
        let host_b_path = "D:\\Rust\\bin\\rustc.exe";
        let host_a_authority = [21; 32];
        let host_b_authority = [22; 32];
        assert_ne!(host_a_path, host_b_path);
        assert_ne!(host_a_authority, host_b_authority);
        let same_portable_invocation = CompilerInvocationRecipeV2::new(
            invocation.profile(),
            invocation.stage(),
            invocation.tool(),
            invocation.toolchain(),
            invocation.environment(),
            invocation.target_platform(),
            invocation.options_digest(),
        )
        .expect("same portable invocation");
        assert_eq!(invocation.identity(), same_portable_invocation.identity());
    }

    #[test]
    fn v2_offer_matcher_binds_closure_manifest_and_full_workspace_work_id() {
        let manifest = manifest();
        let object_id = manifest.object_id().expect("manifest ID");
        let typed_object = manifest.typed_object().expect("manifest object");
        let closure = backend_store::ClosureManifest::new(vec![typed_object])
            .expect("closure")
            .id();
        let offer = offer(&manifest, closure, object_id);
        assert_eq!(manifest.matches_offer(&offer, closure), Ok(true));

        let mut swapped = offer.clone();
        swapped.input_closure_id[0] ^= 1;
        assert_eq!(manifest.matches_offer(&swapped, closure), Ok(false));
        let mut wrong_manifest = offer.clone();
        wrong_manifest.input_manifest_object_id[0] ^= 1;
        assert_eq!(manifest.matches_offer(&wrong_manifest, closure), Ok(false));

        let mut wrong_root = offer.clone();
        wrong_root.input_root[0] ^= 1;
        assert_eq!(manifest.matches_offer(&wrong_root, closure), Ok(false));
        let mut wrong_read_manifest = offer.clone();
        wrong_read_manifest.read_manifest[0] ^= 1;
        assert_eq!(
            manifest.matches_offer(&wrong_read_manifest, closure),
            Ok(false)
        );
        let mut wrong_package = offer.clone();
        wrong_package.package_lineage[0] ^= 1;
        assert_eq!(manifest.matches_offer(&wrong_package, closure), Ok(false));
        let mut wrong_target = offer.clone();
        wrong_target.target[0] ^= 1;
        assert_eq!(manifest.matches_offer(&wrong_target, closure), Ok(false));
        let mut wrong_recipe = offer.clone();
        wrong_recipe.recipe[0] ^= 1;
        assert_eq!(manifest.matches_offer(&wrong_recipe, closure), Ok(false));
        let mut stale_work_id = offer.clone();
        stale_work_id.scope.work_id[0] ^= 1;
        assert_eq!(manifest.matches_offer(&stale_work_id, closure), Ok(false));
        let mut incremental = offer;
        incremental.selected_base = Some([12; 32]);
        assert_eq!(manifest.matches_offer(&incremental, closure), Ok(false));
    }
}
