//! Explicit pinned CPU Candle setup. Installing bytes does not activate model code.

use super::{
    LocalEmbeddingTreatment, LocalQueryRecipeContract, LocalQueryRecipeDescriptor,
    LocalQueryRecipeError,
};
use backend_compile::{
    EmbeddingNormalization, EmbeddingRuntimeSpecV1, ExecutableIdentity, ProcessError,
    ToolchainArtifact,
};
use backend_engine::application::{
    EmbeddingRequirement, EmbeddingRuntimeError, EmbeddingRuntimeInstall, EmbeddingRuntimeLimits,
    inspect_embedding_runtime, install_embedding_runtime_with_identity,
};
use backend_extension_qdrant as vector;
use backend_platform::DirectoryCapability;
use std::{
    fmt,
    io::{self, Read},
    num::{NonZeroU16, NonZeroU32},
    path::{Path, PathBuf},
};

pub(super) const MODEL_BYTES: usize = 90_868_376;
pub(super) const TOKENIZER_BYTES: usize = 466_247;
pub(super) const MODEL_HASH: [u8; 32] = [
    0x80, 0x87, 0xe9, 0xbf, 0x97, 0xc2, 0x65, 0xf8, 0x43, 0x5e, 0xd2, 0x68, 0x73, 0x3e, 0xcf, 0x37,
    0x91, 0x82, 0x5a, 0xd2, 0x48, 0x50, 0xfd, 0x5d, 0x84, 0xd8, 0x9e, 0x32, 0xee, 0x3a, 0x58, 0x9a,
];
pub(super) const TOKENIZER_HASH: [u8; 32] = [
    0x82, 0x48, 0x3b, 0xb4, 0xf0, 0xbd, 0xb8, 0x17, 0x79, 0xf2, 0x95, 0xec, 0xc5, 0xa9, 0x32, 0x85,
    0xd2, 0x15, 0x68, 0x34, 0xe9, 0x94, 0xa2, 0x16, 0x9f, 0x98, 0x00, 0xe4, 0xc8, 0xf2, 0x50, 0xc1,
];
// These declarations describe the actual pinned helper source, not an arbitrary executable.
const PROVIDER_OPTIONS: &[u8] = b"nudox.candle-minilm.cpu.v1\0candle=0.11.0;tokenizers=0.23.1;bert=6x384x12-vocab30522-f32;utf8=unmodified;special_tokens=true;normalizer=artifact;truncation=256-right-longest-first-stride0;export_padding=disabled;microbatch=16;dynamic_padding=right-zero-mask;pool=attended-mean-including-cls-sep;normalization=f32-l2";

/// Which exact pinned source artifact refused admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandleArtifact {
    Model,
    Tokenizer,
}

/// Closed setup refusal, preserving original errors without interpreting their text.
#[derive(Debug)]
pub enum CandleSetupError {
    Io(io::Error),
    Process(ProcessError),
    Runtime(EmbeddingRuntimeError),
    Recipe(LocalQueryRecipeError),
    InvalidPath,
    ArtifactLength {
        artifact: CandleArtifact,
        observed: u64,
        expected: usize,
    },
    ArtifactIdentity(CandleArtifact),
    HelperIdentity,
    RequiredDowngrade,
    ResourcePolicy,
    SetupBusy,
}
impl fmt::Display for CandleSetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f), Self::Process(error) => error.fmt(f),
            Self::Runtime(error) => error.fmt(f), Self::Recipe(error) => error.fmt(f),
            Self::InvalidPath => f.write_str("setup requires absolute regular artifact paths without symbolic links"),
            Self::ArtifactLength { artifact, observed, expected } => write!(f,"pinned {artifact:?} length is {observed}, expected {expected}"),
            Self::ArtifactIdentity(artifact) => write!(f,"pinned {artifact:?} bytes do not match the source-owned identity"),
            Self::HelperIdentity => f.write_str("helper/dependency identity differs from the explicitly trusted build receipt"),
            Self::RequiredDowngrade => f.write_str("setup cannot downgrade an existing required embedding runtime to optional"),
            Self::ResourcePolicy => f.write_str("CPU setup requires an explicit nonzero child memory ceiling and valid shared owner credits"),
            Self::SetupBusy => f.write_str("another embedding setup operation holds this workspace's install lock"),
        }
    }
}
impl std::error::Error for CandleSetupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Process(e) => Some(e),
            Self::Runtime(e) => Some(e),
            Self::Recipe(e) => Some(e),
            _ => None,
        }
    }
}

/// Exact caller-supplied artifacts and explicit resource envelope; no ambient environment.
pub struct PinnedCandleSetupRequest {
    pub program: PathBuf,
    pub dependencies: Vec<PathBuf>,
    pub expected_toolchain: [u8; 32],
    pub model: PathBuf,
    pub tokenizer: PathBuf,
    pub requirement: EmbeddingRequirement,
    pub limits: EmbeddingRuntimeLimits,
}

/// Prepared immutable bytes and exact trusted launch identity. No model was executed.
pub struct PreparedCandleSetup {
    request: EmbeddingRuntimeInstall,
    helper: ToolchainArtifact,
    contract: LocalQueryRecipeContract,
}

/// Observes a helper's exact declared closure, without executing it or claiming its semantics.
pub fn inspect_candle_helper(
    program: &Path,
    dependencies: &[PathBuf],
) -> Result<[u8; 32], CandleSetupError> {
    Ok(helper(program, dependencies)?.identity().to_bytes())
}
fn helper(program: &Path, dependencies: &[PathBuf]) -> Result<ToolchainArtifact, CandleSetupError> {
    if !program.is_absolute() || dependencies.len() > 64 {
        return Err(CandleSetupError::InvalidPath);
    }
    let mut paths = std::collections::BTreeSet::new();
    let mut ids = Vec::new();
    for path in dependencies {
        if !path.is_absolute() || path == program || !paths.insert(path) {
            return Err(CandleSetupError::InvalidPath);
        }
        ids.push(
            ExecutableIdentity::from_path(path)
                .map_err(CandleSetupError::Process)?
                .digest(),
        );
    }
    ToolchainArtifact::from_path(program, ids).map_err(CandleSetupError::Process)
}

impl PreparedCandleSetup {
    /// Verifies the pinned model/tokenizer and caller-trusted helper closure without starting it.
    pub fn prepare(request: PinnedCandleSetupRequest) -> Result<Self, CandleSetupError> {
        admit_candle_setup_authority(
            &request.program,
            &request.dependencies,
            request.expected_toolchain,
            request.limits,
        )?;
        let dimensions = NonZeroU16::new(384).ok_or(CandleSetupError::ResourcePolicy)?;
        let maximum_text = NonZeroU32::new(64 * 1024).ok_or(CandleSetupError::ResourcePolicy)?;
        request
            .limits
            .validate(
                MODEL_BYTES as u64,
                TOKENIZER_BYTES as u64,
                dimensions,
                maximum_text,
            )
            .map_err(CandleSetupError::Runtime)?;
        let helper = helper(&request.program, &request.dependencies)?;
        if helper.identity().to_bytes() != request.expected_toolchain {
            return Err(CandleSetupError::HelperIdentity);
        }
        let model_bytes = read_artifact(
            &request.model,
            CandleArtifact::Model,
            MODEL_BYTES,
            MODEL_HASH,
        )?;
        let tokenizer_bytes = read_artifact(
            &request.tokenizer,
            CandleArtifact::Tokenizer,
            TOKENIZER_BYTES,
            TOKENIZER_HASH,
        )?;
        helper
            .verify_path(&request.program)
            .map_err(CandleSetupError::Process)?;
        let contract = LocalQueryRecipeContract {
            metric: vector::Metric::CosineDistance,
            pooling: vector::EmbeddingPooling::Mean,
            encoding: vector::EmbeddingEncoding::Float32,
            query_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
            document_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
            provider_options: *blake3::hash(PROVIDER_OPTIONS).as_bytes(),
        };
        Ok(Self {
            request: EmbeddingRuntimeInstall {
                requirement: request.requirement,
                program: request.program,
                arguments: Vec::new(),
                dependencies: request.dependencies,
                environment: vec![("BACKEND_EMBEDDING_DEVICE".into(), "cpu".into())],
                limits: request.limits,
                model_bytes,
                tokenizer_bytes,
                dimension: dimensions,
                normalization: EmbeddingNormalization::L2,
                maximum_text_bytes: maximum_text,
                options_digest: contract.options_digest(),
            },
            helper,
            contract,
        })
    }

    /// Publishes an immutable recipe descriptor before the sole runtime config pointer.
    /// Descriptor failure preserves the previous config; interruption can leave an unreferenced
    /// descriptor, while the previous runtime still selects its own exact descriptor.
    pub fn install(self, compiler: &Path) -> Result<EmbeddingRuntimeSpecV1, CandleSetupError> {
        self.request
            .limits
            .validate_platform()
            .map_err(CandleSetupError::Runtime)?;
        backend_platform::durable::ensure_private_directory(compiler)
            .map_err(CandleSetupError::Io)?;
        let directory = DirectoryCapability::open(compiler).map_err(CandleSetupError::Io)?;
        let lock = directory
            .open_private_file_read_write("embedding-setup.lock", true)
            .map_err(CandleSetupError::Io)?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => CandleSetupError::SetupBusy,
            std::fs::TryLockError::Error(error) => CandleSetupError::Io(error),
        })?;
        let config = compiler.join("embedding.config");
        if inspect_embedding_runtime(&config)
            .map_err(CandleSetupError::Runtime)?
            .is_some_and(|existing| {
                existing.requirement == EmbeddingRequirement::Required
                    && self.request.requirement == EmbeddingRequirement::Optional
            })
        {
            return Err(CandleSetupError::RequiredDowngrade);
        }
        self.helper
            .verify_path(&self.request.program)
            .map_err(CandleSetupError::Process)?;
        let model_hash = *blake3::hash(&self.request.model_bytes).as_bytes();
        let expected_spec = EmbeddingRuntimeSpecV1::new(
            model_hash,
            model_hash,
            *blake3::hash(&self.request.tokenizer_bytes).as_bytes(),
            self.helper.identity().to_bytes(),
            self.request.dimension,
            self.request.normalization,
            self.request.maximum_text_bytes,
            self.request.options_digest,
        );
        let descriptor = LocalQueryRecipeDescriptor::new(expected_spec, self.contract)
            .map_err(CandleSetupError::Recipe)?;
        backend_platform::durable::write_private_atomic(
            &super::local_recipe::recipe_descriptor_path(compiler, expected_spec.recipe_identity()),
            &descriptor.canonical_bytes(),
        )
        .map_err(CandleSetupError::Io)?;
        let spec = install_embedding_runtime_with_identity(
            &config,
            self.request,
            self.helper.identity().to_bytes(),
        )
        .map_err(CandleSetupError::Runtime)?;
        Ok(spec)
    }
}

/// Admits explicit trusted helper identity and resource policy before acquiring model bytes.
/// This reads toolchain bytes but starts no process and attests no executable semantics.
pub fn admit_candle_setup_authority(
    program: &Path,
    dependencies: &[PathBuf],
    expected_toolchain: [u8; 32],
    limits: EmbeddingRuntimeLimits,
) -> Result<(), CandleSetupError> {
    limits
        .validate_platform()
        .map_err(CandleSetupError::Runtime)?;
    if !limits.memory_bytes.is_some_and(|bytes| bytes > 0) {
        return Err(CandleSetupError::ResourcePolicy);
    }
    limits
        .validate(
            MODEL_BYTES as u64,
            TOKENIZER_BYTES as u64,
            NonZeroU16::new(384).ok_or(CandleSetupError::ResourcePolicy)?,
            NonZeroU32::new(64 * 1024).ok_or(CandleSetupError::ResourcePolicy)?,
        )
        .map_err(CandleSetupError::Runtime)?;
    if helper(program, dependencies)?.identity().to_bytes() != expected_toolchain {
        return Err(CandleSetupError::HelperIdentity);
    }
    Ok(())
}
fn read_artifact(
    path: &Path,
    artifact: CandleArtifact,
    expected: usize,
    digest: [u8; 32],
) -> Result<Vec<u8>, CandleSetupError> {
    if !path.is_absolute() {
        return Err(CandleSetupError::InvalidPath);
    }
    let parent = path.parent().ok_or(CandleSetupError::InvalidPath)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(CandleSetupError::InvalidPath)?;
    let directory =
        DirectoryCapability::open_read_only_source(parent).map_err(CandleSetupError::Io)?;
    let mut file = directory
        .open_file_read(name)
        .map_err(CandleSetupError::Io)?;
    let length = file.metadata().map_err(CandleSetupError::Io)?.len();
    if length != expected as u64 {
        return Err(CandleSetupError::ArtifactLength {
            artifact,
            observed: length,
            expected,
        });
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected)
        .map_err(|_| CandleSetupError::ResourcePolicy)?;
    file.by_ref()
        .take(expected as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(CandleSetupError::Io)?;
    if bytes.len() != expected {
        return Err(CandleSetupError::ArtifactLength {
            artifact,
            observed: bytes.len() as u64,
            expected,
        });
    }
    if blake3::hash(&bytes).as_bytes() != &digest {
        return Err(CandleSetupError::ArtifactIdentity(artifact));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    type TestResult = Result<(), Box<dyn std::error::Error>>;
    fn limits() -> EmbeddingRuntimeLimits {
        EmbeddingRuntimeLimits {
            stdout_bytes: 1024 * 1024,
            stderr_bytes: 1024,
            output_bytes: 1024 * 1024 + 1024,
            input_bytes: 1024 * 1024,
            wall_time_ms: 30_000,
            workspace_bytes: 128 * 1024 * 1024,
            process_count: 4,
            memory_bytes: Some(128 * 1024 * 1024),
            cpu_time_ms: Some(30_000),
            resident_credit_bytes: 1024 * 1024 * 1024,
        }
    }
    fn fixture_helper(directory: &Path) -> Result<PathBuf, io::Error> {
        let program = directory.join("fixture-helper");
        std::fs::write(&program, b"unit identity bytes; never execute as Candle")?;
        Ok(program)
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_setup_refuses_untrusted_helper_and_resources_before_model_read() -> TestResult {
        let directory = tempfile::tempdir()?;
        let program = fixture_helper(directory.path())?;
        let observed = inspect_candle_helper(&program, &[])?;
        let request = |policy, identity| PinnedCandleSetupRequest {
            program: program.clone(),
            dependencies: Vec::new(),
            expected_toolchain: identity,
            model: directory.path().join("missing-model"),
            tokenizer: directory.path().join("missing-tokenizer"),
            requirement: EmbeddingRequirement::Optional,
            limits: policy,
        };
        let mut missing_limit = limits();
        missing_limit.memory_bytes = None;
        assert!(matches!(
            PreparedCandleSetup::prepare(request(missing_limit, observed)),
            Err(CandleSetupError::ResourcePolicy)
        ));
        let mut invalid_limit = limits();
        invalid_limit.wall_time_ms = 0;
        assert!(matches!(
            PreparedCandleSetup::prepare(request(invalid_limit, observed)),
            Err(CandleSetupError::Runtime(
                EmbeddingRuntimeError::InvalidConfig
            ))
        ));
        assert!(matches!(
            PreparedCandleSetup::prepare(request(limits(), [0; 32])),
            Err(CandleSetupError::HelperIdentity)
        ));
        assert!(matches!(
            PreparedCandleSetup::prepare(request(limits(), observed)),
            Err(CandleSetupError::Io(_))
        ));
        assert!(!directory.path().join("embedding.config").exists());
        Ok(())
    }
    #[test]
    fn source_artifact_bounds_hashes_and_symlinks_refuse_before_publication() -> TestResult {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("artifact");
        std::fs::write(&file, b"abc")?;
        assert!(matches!(
            read_artifact(&file, CandleArtifact::Model, 4, [0; 32]),
            Err(CandleSetupError::ArtifactLength {
                artifact: CandleArtifact::Model,
                observed: 3,
                expected: 4
            })
        ));
        assert!(matches!(
            read_artifact(&file, CandleArtifact::Tokenizer, 3, [0; 32]),
            Err(CandleSetupError::ArtifactIdentity(
                CandleArtifact::Tokenizer
            ))
        ));
        assert_eq!(
            read_artifact(
                &file,
                CandleArtifact::Model,
                3,
                *blake3::hash(b"abc").as_bytes()
            )?,
            b"abc"
        );
        assert!(matches!(
            read_artifact(Path::new("relative"), CandleArtifact::Model, 3, [0; 32]),
            Err(CandleSetupError::InvalidPath)
        ));
        #[cfg(unix)]
        {
            let link = directory.path().join("link");
            std::os::unix::fs::symlink(&file, &link)?;
            assert!(matches!(
                read_artifact(
                    &link,
                    CandleArtifact::Model,
                    3,
                    *blake3::hash(b"abc").as_bytes()
                ),
                Err(CandleSetupError::Io(_))
            ));
        }
        Ok(())
    }
    // Private fixture bypass tests publication/required-policy laws only. It is not pinned model
    // admission and never calls activation/probe/inference or credits genuine semantic availability.
    fn prepared_publication_fixture(
        directory: &Path,
        requirement: EmbeddingRequirement,
    ) -> Result<PreparedCandleSetup, CandleSetupError> {
        let program = fixture_helper(directory).map_err(CandleSetupError::Io)?;
        let helper = helper(&program, &[])?;
        let contract = LocalQueryRecipeContract {
            metric: vector::Metric::CosineDistance,
            pooling: vector::EmbeddingPooling::Mean,
            encoding: vector::EmbeddingEncoding::Float32,
            query_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
            document_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
            provider_options: *blake3::hash(b"publication fixture, not Candle profile").as_bytes(),
        };
        let request = EmbeddingRuntimeInstall {
            requirement,
            program,
            arguments: Vec::new(),
            dependencies: Vec::new(),
            environment: Vec::new(),
            limits: limits(),
            model_bytes: b"fixture model".to_vec(),
            tokenizer_bytes: b"fixture tokenizer".to_vec(),
            dimension: NonZeroU16::new(2).ok_or(CandleSetupError::ResourcePolicy)?,
            normalization: EmbeddingNormalization::L2,
            maximum_text_bytes: NonZeroU32::new(64).ok_or(CandleSetupError::ResourcePolicy)?,
            options_digest: contract.options_digest(),
        };
        Ok(PreparedCandleSetup {
            request,
            helper,
            contract,
        })
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn setup_publication_preserves_required_policy_and_matching_descriptor_without_activation()
    -> TestResult {
        let directory = tempfile::tempdir()?;
        let compiler = directory.path().join("compiler");
        let prepared =
            prepared_publication_fixture(directory.path(), EmbeddingRequirement::Required)?;
        let installed = prepared.install(&compiler)?;
        let summary = inspect_embedding_runtime(compiler.join("embedding.config"))?
            .ok_or("persisted spec")?;
        assert_eq!(summary.spec, installed);
        assert_eq!(summary.requirement, EmbeddingRequirement::Required);
        let descriptor_path = super::super::local_recipe::recipe_descriptor_path(
            &compiler,
            installed.recipe_identity(),
        );
        let bytes = std::fs::read(&descriptor_path)?;
        let declared = LocalQueryRecipeDescriptor::decode(&bytes)?;
        assert_eq!(declared.canonical_bytes(), bytes);
        let before = std::fs::read(compiler.join("embedding.config"))?;
        let optional =
            prepared_publication_fixture(directory.path(), EmbeddingRequirement::Optional)?;
        assert!(matches!(
            optional.install(&compiler),
            Err(CandleSetupError::RequiredDowngrade)
        ));
        assert_eq!(std::fs::read(compiler.join("embedding.config"))?, before);
        assert_eq!(std::fs::read(&descriptor_path)?, bytes);
        Ok(())
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn changed_prepared_dependency_never_publishes_runtime_or_descriptor() -> TestResult {
        let directory = tempfile::tempdir()?;
        let compiler = directory.path().join("compiler");
        let installed =
            prepared_publication_fixture(directory.path(), EmbeddingRequirement::Optional)?;
        let old_spec = installed.install(&compiler)?;
        let before_config = std::fs::read(compiler.join("embedding.config"))?;
        let old_descriptor = super::super::local_recipe::recipe_descriptor_path(
            &compiler,
            old_spec.recipe_identity(),
        );
        let before_descriptor = std::fs::read(&old_descriptor)?;
        let mut prepared =
            prepared_publication_fixture(directory.path(), EmbeddingRequirement::Optional)?;
        let dependency = directory.path().join("dependency");
        std::fs::write(&dependency, b"admitted dependency")?;
        prepared.request.dependencies = vec![dependency.clone()];
        prepared.helper = helper(&prepared.request.program, &prepared.request.dependencies)?;
        std::fs::write(&dependency, b"changed dependency")?;
        assert!(matches!(
            prepared.install(&compiler),
            Err(CandleSetupError::Runtime(
                EmbeddingRuntimeError::ExecutableIdentityMismatch
            ))
        ));
        assert_eq!(
            std::fs::read(compiler.join("embedding.config"))?,
            before_config
        );
        assert_eq!(std::fs::read(&old_descriptor)?, before_descriptor);
        Ok(())
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn immutable_descriptor_write_failure_preserves_previous_runtime_selection() -> TestResult {
        let directory = tempfile::tempdir()?;
        let compiler = directory.path().join("compiler");
        let old = prepared_publication_fixture(directory.path(), EmbeddingRequirement::Optional)?
            .install(&compiler)?;
        let old_descriptor =
            super::super::local_recipe::recipe_descriptor_path(&compiler, old.recipe_identity());
        let config_before = std::fs::read(compiler.join("embedding.config"))?;
        let descriptor_before = std::fs::read(&old_descriptor)?;
        let mut new =
            prepared_publication_fixture(directory.path(), EmbeddingRequirement::Optional)?;
        new.request.model_bytes.push(1);
        let hash = *blake3::hash(&new.request.model_bytes).as_bytes();
        let spec = EmbeddingRuntimeSpecV1::new(
            hash,
            hash,
            *blake3::hash(&new.request.tokenizer_bytes).as_bytes(),
            new.helper.identity().to_bytes(),
            new.request.dimension,
            new.request.normalization,
            new.request.maximum_text_bytes,
            new.request.options_digest,
        );
        let refused_path =
            super::super::local_recipe::recipe_descriptor_path(&compiler, spec.recipe_identity());
        std::fs::create_dir(&refused_path)?;
        assert!(matches!(
            new.install(&compiler),
            Err(CandleSetupError::Io(_))
        ));
        assert_eq!(
            std::fs::read(compiler.join("embedding.config"))?,
            config_before
        );
        assert_eq!(std::fs::read(&old_descriptor)?, descriptor_before);
        assert_eq!(
            inspect_embedding_runtime(compiler.join("embedding.config"))?
                .ok_or("old configuration")?
                .spec,
            old
        );
        Ok(())
    }
    #[test]
    fn unsupported_preset_platform_refuses_before_artifact_acquisition() {
        let result = limits().validate_platform();
        if cfg!(target_os = "linux") {
            assert!(result.is_ok());
        } else {
            assert!(matches!(
                result,
                Err(EmbeddingRuntimeError::Process(
                    ProcessError::UnsupportedLimit(_)
                ))
            ));
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn setup_install_lock_refuses_concurrent_publication() -> TestResult {
        let directory = tempfile::tempdir()?;
        let compiler = directory.path().join("compiler");
        backend_platform::durable::ensure_private_directory(&compiler)?;
        let parent = DirectoryCapability::open(&compiler)?;
        let lock = parent.open_private_file_read_write("embedding-setup.lock", true)?;
        lock.try_lock()?;
        let prepared =
            prepared_publication_fixture(directory.path(), EmbeddingRequirement::Optional)?;
        assert!(matches!(
            prepared.install(&compiler),
            Err(CandleSetupError::SetupBusy)
        ));
        assert!(!compiler.join("embedding.config").exists());
        assert!(!compiler.join("embedding-query.config").exists());
        Ok(())
    }
}
