//! Authenticated lifecycle for downloadable language oracles and embedding models.
//!
//! The lifecycle is deliberately linear:
//! `UntrustedManifest -> Available -> Installed -> Resident -> Active -> ExecutionReady`.
//! Each transition requires evidence owned by the lower layer.  In particular, a placement
//! observation cannot manufacture an active capability: readiness borrows a live runtime and the
//! immutable store object that supplied its bytes.

use std::{fmt, num::NonZeroU64, sync::Arc};

use backend_store::TypedObject;
use backend_version::{ObjectKey, ObjectVersion, Schema};
pub use backend_semantic::vocabulary::LanguageOracleTask;

/// Raw bytes of a downloadable capability artifact.
pub struct CapabilityArtifactSchema;

impl Schema for CapabilityArtifactSchema {
    const DOMAIN: u8 = 0x63;
    const TYPE: u16 = 0x0100;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Canonical manifest identity schema.
pub struct CapabilityManifestSchema;

impl Schema for CapabilityManifestSchema {
    const DOMAIN: u8 = 0x63;
    const TYPE: u16 = 0x0101;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Canonical embedding-space recipe identity schema.
pub struct EmbeddingRecipeSchema;

impl Schema for EmbeddingRecipeSchema {
    const DOMAIN: u8 = 0x63;
    const TYPE: u16 = 0x0102;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Exact immutable artifact identity derived from its canonical bytes.
pub type CapabilityArtifactId = ObjectVersion<CapabilityArtifactSchema>;
/// Identity of every execution-relevant manifest field.
pub type CapabilityManifestId = ObjectVersion<CapabilityManifestSchema>;
/// Identity of every fact defining an embedding vector space.
pub type EmbeddingRecipeId = ObjectVersion<EmbeddingRecipeSchema>;

macro_rules! digest_identity {
    ($(#[$attribute:meta])* $name:ident) => {
        $(#[$attribute])*
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Admits an identity already checked by the transport/authentication adapter.
            #[must_use]
            pub const fn new(bytes: [u8; 32]) -> Self { Self(bytes) }
            /// Returns its complete fixed-width representation.
            #[must_use]
            pub const fn as_bytes(self) -> [u8; 32] { self.0 }
        }
    };
}

digest_identity!(
    /// Stable artifact origin identity; credentials are deliberately excluded.
    ArtifactSourceId
);
digest_identity!(
    /// Stable authorization-policy scope identity; secret material is excluded.
    AuthorizationScopeId
);
digest_identity!(
    /// Trust root that authenticated the manifest statement.
    TrustRootId
);
digest_identity!(
    /// Exact tokenizer artifact and configuration identity.
    TokenizerId
);
digest_identity!(
    /// Exact query/document prefix or template identity.
    TreatmentId
);

/// Artifact source and authorization policy retained by the authenticated manifest.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactAuthority {
    /// Stable source endpoint/repository identity.
    pub source: ArtifactSourceId,
    /// Credential scope required by policy, or `None` for a public source.
    pub authorization: Option<AuthorizationScopeId>,
    /// Trust root that signed or otherwise authenticated the manifest.
    pub trust_root: TrustRootId,
}

/// Closed operating-system compatibility axis.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OperatingSystem {
    /// Linux userspace.
    Linux,
    /// macOS userspace.
    MacOs,
    /// Windows userspace.
    Windows,
}

/// Closed CPU architecture compatibility axis.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Architecture {
    /// 64-bit Arm.
    Aarch64,
    /// 64-bit x86.
    X86_64,
}

/// Managed/portable runtime required by a capability artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ManagedRuntime {
    /// WebAssembly System Interface preview 1.
    WasiPreview1,
    /// Java virtual machine.
    Java,
    /// .NET runtime.
    DotNet,
    /// Node.js runtime.
    Node,
    /// Python runtime.
    Python,
    /// ONNX model runtime.
    Onnx,
}

/// Exact runtime target for an artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityTarget {
    /// Native executable for exactly one OS/architecture pair.
    Native {
        /// Required operating system.
        os: OperatingSystem,
        /// Required CPU architecture.
        architecture: Architecture,
    },
    /// Portable artifact interpreted by a managed runtime.
    Managed(ManagedRuntime),
}

/// Host facts checked before bytes are acquired or loaded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityHost {
    /// Host operating system.
    pub os: OperatingSystem,
    /// Host CPU architecture.
    pub architecture: Architecture,
    /// Installed managed runtimes.
    pub managed: &'static [ManagedRuntime],
    /// Capability protocol ABI implemented by this process.
    pub protocol_abi: u16,
}

/// Role of an immutable artifact dependency.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DependencyRole {
    /// Executable or shared-library dependency.
    Runtime,
    /// Language SDK, standard library, or schema data.
    Toolchain,
    /// Tokenizer vocabulary/configuration artifact.
    Tokenizer,
    /// Model sidecar/configuration artifact.
    ModelSidecar,
}

/// One exact member of the capability artifact closure.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactDependency {
    /// Immutable dependency bytes.
    pub artifact: CapabilityArtifactId,
    /// Meaning of this dependency to the loader.
    pub role: DependencyRole,
}

#[path = "capability/recipes.rs"]
mod recipes;
pub use recipes::{
    Chunking, DocumentTreatment, EmbeddingMetric, EmbeddingModelRecipe, LanguageOracleRecipe,
    Normalization, NumericRepresentation, Pooling, QueryTreatment, SourceExtraction,
};

mod private {
    pub trait Sealed {}
}

/// Capability family used to brand every manifest and lifecycle owner.
pub trait CapabilityKind: private::Sealed + Clone + fmt::Debug + Send + Sync + 'static {
    /// Family-specific execution recipe.
    type Recipe: Clone + fmt::Debug + Eq + Send + Sync;
    /// Stable family code included in manifest identity.
    const CODE: u8;
    /// Encodes every execution-relevant recipe field.
    fn encode_recipe(recipe: &Self::Recipe, output: &mut Vec<u8>);
    /// Checks family-specific agreement with the loader ABI.
    fn recipe_matches_abi(_recipe: &Self::Recipe, _abi: u16) -> bool {
        true
    }
}

/// Language-oracle capability brand.
#[derive(Clone, Debug)]
pub enum LanguageOracle {}
impl private::Sealed for LanguageOracle {}
impl CapabilityKind for LanguageOracle {
    type Recipe = LanguageOracleRecipe;
    const CODE: u8 = 1;
    fn encode_recipe(recipe: &Self::Recipe, output: &mut Vec<u8>) {
        output.extend_from_slice(&<[u8; 2]>::from(recipe.profile));
        output.extend_from_slice(&[recipe.task as u8, u8::from(recipe.native_tool)]);
        output.extend_from_slice(&recipe.protocol.to_be_bytes());
        output.extend_from_slice(&recipe.toolchain.to_bytes());
        output.extend_from_slice(&recipe.package_authority);
    }
    fn recipe_matches_abi(recipe: &Self::Recipe, abi: u16) -> bool {
        recipe.protocol == abi
    }
}

/// Embedding-model capability brand.
#[derive(Clone, Debug)]
pub enum EmbeddingModel {}
impl private::Sealed for EmbeddingModel {}
impl CapabilityKind for EmbeddingModel {
    type Recipe = EmbeddingModelRecipe;
    const CODE: u8 = 2;
    fn encode_recipe(recipe: &Self::Recipe, output: &mut Vec<u8>) {
        output.extend_from_slice(recipe.identity().as_bytes());
    }
}

#[path = "capability/inventory.rs"]
mod inventory;
pub use inventory::CapabilityInventoryKind;

/// Manifest fields received from an untrusted catalog or peer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedCapabilityManifest<K: CapabilityKind> {
    /// Exact primary artifact bytes.
    pub artifact: CapabilityArtifactId,
    /// Non-empty primary artifact extent.
    pub bytes: NonZeroU64,
    /// Source/authentication policy.
    pub authority: ArtifactAuthority,
    /// Host/runtime target.
    pub target: CapabilityTarget,
    /// Loader protocol ABI.
    pub protocol_abi: u16,
    /// Complete sorted artifact dependency closure.
    pub dependencies: Vec<ArtifactDependency>,
    /// Family-specific execution recipe.
    pub recipe: K::Recipe,
}

/// Manifest authentication policy. Implementations own signature/key verification.
pub trait CapabilityManifestVerifier<K: CapabilityKind> {
    /// Verification failure.
    type Error;
    /// Authenticates the complete canonical manifest statement under its declared trust root.
    ///
    /// # Errors
    ///
    /// Returns the verifier's exact signature, trust-root, or policy rejection.
    fn verify(
        &self,
        manifest: &UntrustedCapabilityManifest<K>,
        canonical: &[u8],
    ) -> Result<(), Self::Error>;
}

/// Exact manifest admission or compatibility failure.
#[derive(Debug, Eq, PartialEq)]
pub enum ManifestAdmissionError<E> {
    /// Artifact dependency rows were not strictly sorted and unique.
    DependencyOrder,
    /// Language recipe and manifest ABI disagree.
    RecipeProtocol,
    /// Declared authentication policy rejected the statement.
    Authentication(E),
    /// Protocol ABI differs from the host implementation.
    Protocol {
        /// ABI declared by the authenticated manifest.
        required: u16,
        /// ABI implemented by this host.
        available: u16,
    },
    /// Native target does not match this host.
    NativeTarget,
    /// Required managed runtime is unavailable.
    ManagedRuntime(ManagedRuntime),
}

impl<E: fmt::Display> fmt::Display for ManifestAdmissionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DependencyOrder => {
                formatter.write_str("capability dependencies are not sorted and unique")
            }
            Self::RecipeProtocol => {
                formatter.write_str("language recipe protocol differs from manifest ABI")
            }
            Self::Authentication(error) => write!(
                formatter,
                "capability manifest authentication failed: {error}"
            ),
            Self::Protocol {
                required,
                available,
            } => write!(
                formatter,
                "capability ABI {required} is unsupported by host ABI {available}"
            ),
            Self::NativeTarget => formatter.write_str("capability native target is unsupported"),
            Self::ManagedRuntime(runtime) => write!(
                formatter,
                "capability managed runtime is unavailable: {runtime:?}"
            ),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for ManifestAdmissionError<E> {}

/// Authenticated and host-compatible manifest. It is the `Available` lifecycle owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AvailableCapability<K: CapabilityKind> {
    manifest: Arc<CapabilityManifest<K>>,
}

impl<K: CapabilityKind> AvailableCapability<K> {
    /// Authenticates and admits a manifest before acquisition begins.
    ///
    /// # Errors
    ///
    /// Returns an authentication, target, ABI, recipe, or dependency-order rejection.
    pub fn admit<V: CapabilityManifestVerifier<K>>(
        untrusted: UntrustedCapabilityManifest<K>,
        host: CapabilityHost,
        verifier: &V,
    ) -> Result<Self, ManifestAdmissionError<V::Error>> {
        validate_dependencies(&untrusted.dependencies)
            .map_err(|()| ManifestAdmissionError::DependencyOrder)?;
        if !K::recipe_matches_abi(&untrusted.recipe, untrusted.protocol_abi) {
            return Err(ManifestAdmissionError::RecipeProtocol);
        }
        let canonical = canonical_manifest(&untrusted);
        verifier
            .verify(&untrusted, &canonical)
            .map_err(ManifestAdmissionError::Authentication)?;
        if untrusted.protocol_abi != host.protocol_abi {
            return Err(ManifestAdmissionError::Protocol {
                required: untrusted.protocol_abi,
                available: host.protocol_abi,
            });
        }
        check_target(untrusted.target, host)?;
        let identity = ObjectVersion::from_value(canonical.as_slice());
        Ok(Self {
            manifest: Arc::new(CapabilityManifest {
                identity,
                artifact: untrusted.artifact,
                bytes: untrusted.bytes,
                authority: untrusted.authority,
                target: untrusted.target,
                protocol_abi: untrusted.protocol_abi,
                dependencies: untrusted.dependencies.into_boxed_slice(),
                recipe: untrusted.recipe,
            }),
        })
    }

    /// Authenticated manifest identity.
    #[must_use]
    pub fn identity(&self) -> CapabilityManifestId {
        self.manifest.identity
    }
}

#[path = "capability/acquisition.rs"]
mod acquisition;
pub use acquisition::{
    AcquiredCapabilityArtifact, AcquisitionVerificationError, CapabilityAcquisitionVerifier,
    InstallError, VerifiedAcquisition,
};

/// Immutable, authenticated capability manifest. Fields are read-only after admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityManifest<K: CapabilityKind> {
    identity: CapabilityManifestId,
    artifact: CapabilityArtifactId,
    bytes: NonZeroU64,
    authority: ArtifactAuthority,
    target: CapabilityTarget,
    protocol_abi: u16,
    dependencies: Box<[ArtifactDependency]>,
    recipe: K::Recipe,
}

impl<K: CapabilityKind> CapabilityManifest<K> {
    /// Manifest identity, independent of publication/workspace snapshots.
    #[must_use]
    pub const fn identity(&self) -> CapabilityManifestId {
        self.identity
    }
    /// Exact primary artifact identity.
    #[must_use]
    pub const fn artifact(&self) -> CapabilityArtifactId {
        self.artifact
    }
    /// Exact artifact byte extent.
    #[must_use]
    pub const fn bytes(&self) -> NonZeroU64 {
        self.bytes
    }
    /// Authenticated source policy.
    #[must_use]
    pub const fn authority(&self) -> ArtifactAuthority {
        self.authority
    }
    /// Admitted execution target.
    #[must_use]
    pub const fn target(&self) -> CapabilityTarget {
        self.target
    }
    /// Admitted loader ABI.
    #[must_use]
    pub const fn protocol_abi(&self) -> u16 {
        self.protocol_abi
    }
    /// Complete dependency closure.
    #[must_use]
    pub fn dependencies(&self) -> &[ArtifactDependency] {
        &self.dependencies
    }
    /// Family-specific execution recipe.
    #[must_use]
    pub const fn recipe(&self) -> &K::Recipe {
        &self.recipe
    }
}

/// Installed capability backed by an immutable verified store object.
#[derive(Clone, Debug)]
pub struct InstalledCapability<K: CapabilityKind> {
    manifest: Arc<CapabilityManifest<K>>,
    artifact: Arc<TypedObject>,
}

impl<K: CapabilityKind> InstalledCapability<K> {
    /// Manifest retained by this installation.
    #[must_use]
    pub fn manifest(&self) -> &CapabilityManifest<K> {
        &self.manifest
    }
    /// Acquires an owned in-memory residence lease. Multiple executions may coexist safely.
    #[must_use]
    pub fn resident(&self) -> ResidentCapability<K> {
        ResidentCapability {
            manifest: Arc::clone(&self.manifest),
            artifact: Arc::clone(&self.artifact),
        }
    }
}

/// Verified resident artifact lease required by runtime activation.
#[derive(Debug)]
pub struct ResidentCapability<K: CapabilityKind> {
    manifest: Arc<CapabilityManifest<K>>,
    artifact: Arc<TypedObject>,
}

impl<K: CapabilityKind> ResidentCapability<K> {
    /// Admitted manifest.
    #[must_use]
    pub fn manifest(&self) -> &CapabilityManifest<K> {
        &self.manifest
    }
    /// Exact immutable resident bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.artifact.bytes()
    }

    /// Activates a kind-matched runtime and requires a real initial readiness probe.
    ///
    /// # Errors
    ///
    /// Returns the resident lease together with the exact activation or readiness failure.
    pub fn activate<R: CapabilityRuntime<K>>(
        self,
        loader: &R::Loader,
    ) -> Result<ActiveCapability<K, R>, ActivationFailure<K, R>> {
        let mut runtime = match R::activate(&self, loader) {
            Ok(runtime) => runtime,
            Err(error) => {
                return Err(ActivationFailure {
                    resident: self,
                    error: ActivationError::Start(error),
                });
            }
        };
        if let Err(probe) = runtime.probe_ready(&self) {
            let revoke = runtime.revoke().err();
            return Err(ActivationFailure {
                resident: self,
                error: ActivationError::Readiness { probe, revoke },
            });
        }
        Ok(ActiveCapability {
            state: ActiveState::Live {
                resident: self,
                runtime,
            },
        })
    }
}

/// Kind-specific executable/model runtime.
pub trait CapabilityRuntime<K: CapabilityKind>: Sized {
    /// Loader/configuration owner. It may contain process or model-runtime services.
    type Loader;
    /// Exact activation/probe/revocation error.
    type Error;
    /// Creates a runtime from the admitted manifest and verified resident bytes.
    ///
    /// # Errors
    ///
    /// Returns the runtime-specific construction or artifact-binding failure.
    fn activate(
        resident: &ResidentCapability<K>,
        loader: &Self::Loader,
    ) -> Result<Self, Self::Error>;
    /// Performs a live protocol handshake, process check, or model self-test.
    ///
    /// # Errors
    ///
    /// Returns the runtime-specific live probe failure.
    fn probe_ready(&mut self, resident: &ResidentCapability<K>) -> Result<(), Self::Error>;
    /// Stops the process/session/model runtime.
    ///
    /// # Errors
    ///
    /// Returns the runtime-specific cleanup failure.
    fn revoke(&mut self) -> Result<(), Self::Error>;
}

/// Activation failure phase.
#[derive(Debug)]
pub enum ActivationError<E> {
    /// Loader could not construct the runtime.
    Start(E),
    /// Initial live probe failed; optional cleanup failure is retained.
    Readiness {
        /// Failure returned by the live readiness probe.
        probe: E,
        /// Cleanup failure, when revocation also failed.
        revoke: Option<E>,
    },
}

/// Failed activation returns the verified resident lease for retry or eviction.
#[derive(Debug)]
pub struct ActivationFailure<K: CapabilityKind, R: CapabilityRuntime<K>> {
    /// Still-valid resident artifact.
    pub resident: ResidentCapability<K>,
    /// Exact runtime failure.
    pub error: ActivationError<R::Error>,
}

/// Active runtime owner. Its constructor is private and its `Drop` revokes the runtime.
pub struct ActiveCapability<K: CapabilityKind, R: CapabilityRuntime<K>> {
    state: ActiveState<K, R>,
}

enum ActiveState<K: CapabilityKind, R: CapabilityRuntime<K>> {
    Live {
        resident: ResidentCapability<K>,
        runtime: R,
    },
    Vacant,
}

impl<K: CapabilityKind, R: CapabilityRuntime<K>> ActiveCapability<K, R> {
    /// Re-probes the live resource and returns the only execution-bearing borrow.
    ///
    /// # Errors
    ///
    /// Returns a live probe failure or reports that a terminal transition invalidated the owner.
    pub fn execution_ready(
        &mut self,
    ) -> Result<ExecutionReady<'_, K, R>, ReadinessError<R::Error>> {
        match &mut self.state {
            ActiveState::Live { resident, runtime } => {
                runtime
                    .probe_ready(resident)
                    .map_err(ReadinessError::Runtime)?;
                Ok(ExecutionReady { resident, runtime })
            }
            ActiveState::Vacant => Err(ReadinessError::Invalidated),
        }
    }

    /// Explicitly revokes the runtime and recovers its verified resident lease.
    ///
    /// # Errors
    ///
    /// Returns the still-active owner if runtime cleanup fails, or an invalidated-state error.
    pub fn revoke(mut self) -> Result<RevokedCapability<K>, RevocationFailure<K, R>> {
        let state = std::mem::replace(&mut self.state, ActiveState::Vacant);
        match state {
            ActiveState::Live {
                resident,
                mut runtime,
            } => match runtime.revoke() {
                Ok(()) => Ok(RevokedCapability { resident }),
                Err(error) => {
                    self.state = ActiveState::Live { resident, runtime };
                    Err(RevocationFailure {
                        active: self,
                        error: RevocationError::Runtime(error),
                    })
                }
            },
            ActiveState::Vacant => Err(RevocationFailure {
                active: self,
                error: RevocationError::Invalidated,
            }),
        }
    }
}

impl<K: CapabilityKind, R: CapabilityRuntime<K>> Drop for ActiveCapability<K, R> {
    fn drop(&mut self) {
        if let ActiveState::Live { runtime, .. } = &mut self.state {
            let _cleanup_result = runtime.revoke();
        }
    }
}

/// Failure to mint a fresh readiness borrow.
#[derive(Debug)]
pub enum ReadinessError<E> {
    /// Live runtime probe failed.
    Runtime(E),
    /// Owner was already consumed by a terminal transition.
    Invalidated,
}

/// Borrowed proof of a fresh live readiness probe.
pub struct ExecutionReady<'active, K: CapabilityKind, R: CapabilityRuntime<K>> {
    resident: &'active ResidentCapability<K>,
    runtime: &'active mut R,
}

impl<K: CapabilityKind, R: CapabilityRuntime<K>> ExecutionReady<'_, K, R> {
    /// Exact manifest whose runtime is executing.
    #[must_use]
    pub fn manifest(&self) -> &CapabilityManifest<K> {
        self.resident.manifest()
    }
    /// Immutable artifact bytes kept resident for the lifetime of this execution borrow.
    #[must_use]
    pub fn artifact_bytes(&self) -> &[u8] {
        self.resident.bytes()
    }
    /// Actual live runtime handle.
    #[must_use]
    pub fn runtime(&mut self) -> &mut R {
        self.runtime
    }
}

/// Failed revocation returns the still-active owner; it never reports a revoked label.
pub struct RevocationFailure<K: CapabilityKind, R: CapabilityRuntime<K>> {
    /// Runtime that failed to revoke.
    pub active: ActiveCapability<K, R>,
    /// Exact runtime failure.
    pub error: RevocationError<R::Error>,
}

/// Exact revocation failure without an impossible-state panic.
#[derive(Debug)]
pub enum RevocationError<E> {
    /// Runtime-specific cleanup failure.
    Runtime(E),
    /// Owner had already entered a terminal transition.
    Invalidated,
}

/// Successfully revoked runtime retaining reusable verified bytes.
#[derive(Debug)]
pub struct RevokedCapability<K: CapabilityKind> {
    resident: ResidentCapability<K>,
}

#[path = "capability/external_embedding.rs"]
mod external_embedding;
pub use external_embedding::{
    ExternalEmbeddingActivationError, ExternalEmbeddingCapability, ExternalEmbeddingLoader,
};
impl<K: CapabilityKind> RevokedCapability<K> {
    /// Recovers the verified resident lease for a later activation.
    #[must_use]
    pub fn into_resident(self) -> ResidentCapability<K> {
        self.resident
    }
}

fn validate_dependencies(dependencies: &[ArtifactDependency]) -> Result<(), ()> {
    if dependencies.windows(2).any(|pair| pair[0] >= pair[1]) {
        Err(())
    } else {
        Ok(())
    }
}

fn check_target<E>(
    target: CapabilityTarget,
    host: CapabilityHost,
) -> Result<(), ManifestAdmissionError<E>> {
    match target {
        CapabilityTarget::Native { os, architecture }
            if os != host.os || architecture != host.architecture =>
        {
            Err(ManifestAdmissionError::NativeTarget)
        }
        CapabilityTarget::Managed(runtime) if !host.managed.contains(&runtime) => {
            Err(ManifestAdmissionError::ManagedRuntime(runtime))
        }
        _ => Ok(()),
    }
}

fn canonical_manifest<K: CapabilityKind>(manifest: &UntrustedCapabilityManifest<K>) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(b"backend.capability.manifest.v1\0");
    output.push(K::CODE);
    output.extend_from_slice(manifest.artifact.as_bytes());
    output.extend_from_slice(&manifest.bytes.get().to_be_bytes());
    output.extend_from_slice(&manifest.authority.source.as_bytes());
    match manifest.authority.authorization {
        None => output.push(0),
        Some(scope) => {
            output.push(1);
            output.extend_from_slice(&scope.as_bytes());
        }
    }
    output.extend_from_slice(&manifest.authority.trust_root.as_bytes());
    match manifest.target {
        CapabilityTarget::Native { os, architecture } => {
            output.extend_from_slice(&[0, os as u8, architecture as u8]);
        }
        CapabilityTarget::Managed(runtime) => output.extend_from_slice(&[1, runtime as u8]),
    }
    output.extend_from_slice(&manifest.protocol_abi.to_be_bytes());
    output.extend_from_slice(&(manifest.dependencies.len() as u64).to_be_bytes());
    for dependency in &manifest.dependencies {
        output.extend_from_slice(dependency.artifact.as_bytes());
        output.push(dependency.role as u8);
    }
    K::encode_recipe(&manifest.recipe, &mut output);
    output
}

/// Builds the exact immutable store object consumed by installation.
///
/// Replication adapters should call this only with bytes admitted by `ReceivingCas::finish`; the
/// resulting private `TypedObject` representation prevents mutable byte-owner substitution.
#[must_use]
pub fn capability_artifact_object(logical_key: &[u8], bytes: &[u8]) -> Arc<TypedObject> {
    let key = ObjectKey::<CapabilityArtifactSchema>::from_value(logical_key);
    Arc::new(TypedObject::from_value(&key, bytes))
}

#[cfg(test)]
#[path = "capability/tests.rs"]
mod tests;
