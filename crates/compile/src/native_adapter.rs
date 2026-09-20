//! Shared native authority adapter mechanics.
//!
//! Language leaves own their input manifests and authority identities.  This
//! module owns the common checked command template, exact request assembly,
//! process execution, coverage admission, and semantic-facet mapping.

use crate::contract::AuthorityRegistry;
use crate::{
    AuthorityError, AuthorityIdentity, CoverageWitness, DiscoverySnapshot, ExecutableIdentity,
    Extraction, FactKeySchema, FactKind, FactRecord, FactValueSchema, InputManifestId,
    NativeAuthorityRunner, NativeCoverage, NativeEnvelope, NativeExit, NativeRequest,
    NativeRequestInput, NativeRunnerError, PreparationCache, PreparationError, PreparationKey,
    PreparedRequest, ProcessEnvironment, ProcessLimits, ProcessStdin, ProtocolDescriptor,
    ScopeRoot, SessionKey, SupervisedCommand, ToolchainId, UntrustedCoverageScope,
};
use backend_semantic::{FacetKind, FacetValue, FacetValueSchema};
use backend_version::ObjectVersion;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Semantic families represented by the normalized native envelope.
///
/// This witness lives with the envelope and extraction protocol so every
/// language leaf binds discovery and request manifests to the same semantic
/// schema. A change to the normalized facet vocabulary therefore invalidates
/// every native authority session together, without compiling a private copy
/// of the helper in each leaf.
const NATIVE_FACETS: [FacetKind; 6] = [
    FacetKind::Source,
    FacetKind::Entity,
    FacetKind::Facet,
    FacetKind::Type,
    FacetKind::Edge,
    FacetKind::Configuration,
];

/// Returns the checked default limit set shared by native lanes.
///
/// # Errors
///
/// Returns [`AuthorityError`] when the platform cannot support the default
/// limit set.
pub fn default_native_limits() -> Result<ProcessLimits, AuthorityError> {
    ProcessLimits::new(256 * 1024, 64 * 1024, Duration::from_secs(10), 320 * 1024)
        .map_err(|error| AuthorityError::Discovery(error.to_string()))
}

/// Returns the canonical semantic schema witness used by every native leaf.
///
/// The witness includes the normalized facet vocabulary and its value-schema
/// version. It is intended for input manifests and native requests; callers
/// must not use a plain string or a digest-only claim for this fence.
#[must_use]
pub fn native_semantic_evidence() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2 + NATIVE_FACETS.len() * 2);
    bytes.push(backend_version::CANONICAL_VERSION);
    bytes.push(<FacetValueSchema as backend_version::Schema>::VERSION);
    for facet in NATIVE_FACETS {
        bytes.extend_from_slice(&facet.tag().to_be_bytes());
    }
    let value = FacetValue::new(FacetKind::Facet, bytes);
    ObjectVersion::<FacetValueSchema>::from_value(&value)
        .to_bytes()
        .to_vec()
}

/// Builds the semantic schema witness input shared by native requests.
///
/// # Errors
///
/// Returns [`AuthorityError`] when the bounded native input contract rejects
/// the witness (which should only happen if protocol limits are tightened
/// below this fixed-size value).
pub fn native_semantic_input() -> Result<NativeRequestInput, AuthorityError> {
    native_input("semantic-fact-schema", native_semantic_evidence())
}

/// Returns canonical evidence for an executable or helper path.
///
/// Present files are identified by their exact content digest. The path is a
/// location used to read the artifact and is deliberately excluded from the
/// returned identity. Missing or changing files retain a deterministic
/// unavailable marker and are classified as unavailable by [`extract_native`].
#[must_use]
pub fn native_executable_evidence(path: &Path) -> Vec<u8> {
    match ExecutableIdentity::from_path(path) {
        Ok(identity) => identity.digest().to_bytes().to_vec(),
        Err(_) => b"NEXE\0unavailable".to_vec(),
    }
}

/// Returns the content-derived toolchain object version for an executable.
///
/// Missing or unreadable paths receive a stable unavailable identity. The
/// frontend manifest separately records the serialized identity bytes, while
/// the checked command retains this typed object version directly.
#[must_use]
pub fn native_executable_id(path: &Path) -> ToolchainId {
    ExecutableIdentity::from_path(path).map_or_else(
        |_| crate::typed_of::<crate::ToolchainSchema>(b"NEXE\0unavailable"),
        |identity| identity.digest(),
    )
}

/// Checked command template retained by a frontend.
#[derive(Clone, Debug)]
pub struct NativeTemplate {
    command: SupervisedCommand,
    helper: PathBuf,
    toolchain: PathBuf,
    protocol: ProtocolDescriptor,
}

/// A native request image prepared once and borrowed through execution.
///
/// The image is retained by the version-keyed [`PreparationCache`]. Keeping
/// the cache guard inside this value makes it impossible to accidentally use
/// a request after its canonical bytes have been evicted or invalidated.
pub struct PreparedNativeInvocation<'a> {
    key: PreparationKey,
    request: PreparedRequest<'a>,
}

impl<'a> PreparedNativeInvocation<'a> {
    fn new(request: PreparedRequest<'a>) -> Self {
        Self {
            key: request.key(),
            request,
        }
    }

    /// Returns the exact version tuple that produced this request image.
    #[must_use]
    pub const fn key(&self) -> PreparationKey {
        self.key
    }

    /// Borrows the canonical request bytes without allocating.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.request.bytes()
    }

    /// Returns an owned reference to the same immutable cache allocation.
    #[must_use]
    pub fn shared_bytes(&self) -> Arc<[u8]> {
        self.request.shared_bytes()
    }
}

enum NativeRequestImage<'a> {
    Borrowed(&'a [u8]),
    Shared(Arc<[u8]>),
}

impl<'a> NativeRequestImage<'a> {
    fn borrowed(bytes: &'a [u8]) -> Self {
        Self::Borrowed(bytes)
    }

    fn shared(bytes: Arc<[u8]>) -> Self {
        Self::Shared(bytes)
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }

    fn stdin(&self) -> ProcessStdin {
        match self {
            Self::Borrowed(bytes) => ProcessStdin::bytes(bytes.to_vec()),
            Self::Shared(bytes) => ProcessStdin::shared_bytes(Arc::clone(bytes)),
        }
    }
}

impl NativeTemplate {
    /// Builds a controlled command template for an absolute helper.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError`] when a helper or toolchain path is not
    /// absolute, the workspace cannot be determined, or the command contract
    /// is invalid.
    pub fn new(
        language: &str,
        helper: impl Into<PathBuf>,
        toolchain_path: impl Into<PathBuf>,
        toolchain: ToolchainId,
        protocol: ProtocolDescriptor,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let helper = helper.into();
        let toolchain_path = toolchain_path.into();
        if !toolchain_path.is_absolute() {
            return Err(AuthorityError::Discovery(
                "native authority toolchain must be absolute".into(),
            ));
        }
        if !helper.is_absolute() {
            return Err(AuthorityError::Discovery(
                "native authority helper must be absolute".into(),
            ));
        }
        let cwd = std::env::current_dir().map_err(|error| {
            AuthorityError::Discovery(format!("native authority workspace unavailable: {error}"))
        })?;
        let environment = ProcessEnvironment::new(vec![
            ("BACKEND_NATIVE_LANGUAGE".into(), language.to_owned()),
            (
                "BACKEND_NATIVE_PROTOCOL".into(),
                protocol.version().to_string(),
            ),
            (
                "BACKEND_NATIVE_TOOLCHAIN".into(),
                toolchain_path.to_string_lossy().into_owned(),
            ),
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
        ])
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        let command = SupervisedCommand::for_authority(
            helper.clone(),
            vec!["--backend-native-authority".into(), language.to_owned()],
            environment,
            cwd,
            ProcessStdin::null(),
            toolchain,
            None,
            protocol,
            limits,
        )
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        Ok(Self {
            command,
            helper,
            toolchain: toolchain_path,
            protocol,
        })
    }

    /// Returns the checked command specification.
    #[must_use]
    pub const fn command(&self) -> &SupervisedCommand {
        &self.command
    }

    /// Returns the exact configured helper path.
    #[must_use]
    pub fn helper(&self) -> &Path {
        &self.helper
    }

    /// Returns the exact absolute toolchain path bound to this helper.
    #[must_use]
    pub fn toolchain(&self) -> &Path {
        &self.toolchain
    }

    /// Returns the advertised helper protocol.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolDescriptor {
        self.protocol
    }

    /// Returns whether this helper advertises persistent sessions.
    #[must_use]
    pub const fn persistent(&self) -> bool {
        self.protocol.supports_persistent()
    }

    fn verify_toolchain(&self) -> Result<(), AuthorityError> {
        let current =
            ExecutableIdentity::from_path(&self.toolchain).map_err(AuthorityError::Process)?;
        if self.command.toolchain() != Some(current.digest()) {
            return Err(AuthorityError::Process(
                crate::ProcessError::ExecutableDrift,
            ));
        }
        Ok(())
    }

    fn command_for(
        &self,
        identity: &AuthorityIdentity,
        key: SessionKey,
        request: Option<ProcessStdin>,
    ) -> Result<SupervisedCommand, AuthorityError> {
        let stdin = match request {
            Some(stdin) => stdin,
            None => ProcessStdin::null(),
        };
        self.command
            .clone()
            .bind_authority(stdin, identity.toolchain, Some(key), self.protocol)
            .map_err(|error| AuthorityError::Discovery(error.to_string()))
    }
}

/// Builds the canonical cold request from exact source/configuration bytes.
///
/// # Errors
///
/// Returns [`AuthorityError`] when an input or the encoded request exceeds the
/// native protocol bounds.
pub fn native_request(
    language: &str,
    key: SessionKey,
    inputs: Vec<NativeRequestInput>,
) -> Result<Vec<u8>, AuthorityError> {
    NativeRequest::new(language, key, inputs)
        .and_then(|request| request.encode())
        .map_err(|error| AuthorityError::Extraction(format!("native request is invalid: {error}")))
}

/// Creates one validated input field for a helper request.
///
/// # Errors
///
/// Returns [`AuthorityError`] when `name` or `bytes` violates native input
/// bounds.
pub fn native_input(
    name: &str,
    bytes: impl Into<Vec<u8>>,
) -> Result<NativeRequestInput, AuthorityError> {
    NativeRequestInput::new(name, bytes)
        .map_err(|error| AuthorityError::Discovery(format!("native input is invalid: {error}")))
}

/// Extracts and admits a helper payload for one discovered snapshot.
///
/// # Errors
///
/// Returns [`AuthorityError`] when the session key, helper response, coverage
/// scope, or native payload is invalid.
pub fn extract_native(
    identity: AuthorityIdentity,
    language: &str,
    snapshot: &DiscoverySnapshot,
    key: SessionKey,
    template: Option<&NativeTemplate>,
    inputs: Vec<NativeRequestInput>,
) -> Result<Extraction, AuthorityError> {
    if !key.matches(&identity, snapshot.manifest()) {
        return Err(AuthorityError::InvalidSessionKey);
    }
    let request = native_request(language, key, inputs)?;
    extract_native_encoded(
        identity,
        language,
        snapshot,
        key,
        template,
        &NativeRequestImage::borrowed(&request),
    )
}

/// Extracts a native payload after rechecking a leaf's current manifest.
///
/// The compile crate owns the stale-snapshot fence and the complete process,
/// session, envelope, and coverage admission path. The leaf supplies only
/// its current manifest digest, allowing a changed source/toolchain/helper or
/// configuration to fail before a child process is spawned.
///
/// # Errors
///
/// Returns [`AuthorityError`] when the snapshot is stale, the session key is
/// invalid, the helper is unavailable, or its bounded output is malformed.
pub fn extract_native_checked(
    identity: AuthorityIdentity,
    language: &str,
    snapshot: &DiscoverySnapshot,
    key: SessionKey,
    template: Option<&NativeTemplate>,
    inputs: Vec<NativeRequestInput>,
    expected_manifest: InputManifestId,
) -> Result<Extraction, AuthorityError> {
    if snapshot.manifest().digest() != expected_manifest {
        return Err(AuthorityError::Extraction(
            "native authority snapshot does not match current frontend inputs".into(),
        ));
    }
    extract_native(identity, language, snapshot, key, template, inputs)
}

/// Extracts through a bounded version-keyed preparation cache.
///
/// A cache hit borrows the canonical request image for the duration of this
/// call.  The cache key includes the authority, toolchain, source manifest,
/// session, and discovery revision, so a changed input or revoked toolchain
/// cannot reuse an older request.  Callers should invalidate the cache when a
/// previously admitted authority is revoked or its executable disappears.
///
/// # Errors
///
/// Returns [`AuthorityError`] when key admission, preparation, or native
/// execution fails.
pub fn extract_native_cached(
    identity: AuthorityIdentity,
    language: &str,
    snapshot: &DiscoverySnapshot,
    key: SessionKey,
    template: Option<&NativeTemplate>,
    inputs: Vec<NativeRequestInput>,
    cache: &PreparationCache,
) -> Result<Extraction, AuthorityError> {
    if !key.matches(&identity, snapshot.manifest()) {
        return Err(AuthorityError::InvalidSessionKey);
    }
    let Some(template) = template else {
        return unsupported(identity, snapshot);
    };
    if !template.helper().exists() {
        let _ = cache.invalidate_authority(identity.digest());
        return unavailable(identity, snapshot);
    }
    if !template.toolchain().exists() {
        let _ = cache.invalidate_toolchain(identity.toolchain);
        return unavailable(identity, snapshot);
    }
    let invocation = prepare_native_invocation(cache, identity, language, snapshot, key, inputs)?;
    let result = extract_native_encoded(
        identity,
        language,
        snapshot,
        key,
        Some(template),
        &NativeRequestImage::shared(invocation.shared_bytes()),
    );
    if matches!(
        &result,
        Err(AuthorityError::Process(
            crate::ProcessError::ExecutableDrift
        ))
    ) {
        let _ = cache.invalidate_toolchain(identity.toolchain);
    }
    result
}

/// Prepares a native invocation under the exact authority, manifest, session,
/// toolchain, and revision key used by extraction.
///
/// This is the single cache boundary for native request construction. Callers
/// may hold the returned invocation across command assembly and process
/// execution; its borrowed bytes remain valid for that lifetime.
///
/// # Errors
///
/// Returns [`AuthorityError::InvalidSessionKey`] when the request key does
/// not match the supplied authority and manifest, or an extraction error when
/// canonical preparation exceeds protocol or cache bounds.
pub fn prepare_native_invocation<'a>(
    cache: &'a PreparationCache,
    identity: AuthorityIdentity,
    language: &str,
    snapshot: &DiscoverySnapshot,
    key: SessionKey,
    inputs: Vec<NativeRequestInput>,
) -> Result<PreparedNativeInvocation<'a>, AuthorityError> {
    if !key.matches(&identity, snapshot.manifest()) {
        return Err(AuthorityError::InvalidSessionKey);
    }
    let preparation_key = PreparationKey::new(
        identity,
        snapshot.manifest().digest(),
        key,
        snapshot.sequence(),
        language,
        &inputs,
    )
    .map_err(|error| AuthorityError::Extraction(error.to_string()))?;
    cache
        .prepare_verified(&preparation_key, language, inputs)
        .map(PreparedNativeInvocation::new)
        .map_err(|error| map_preparation_error(&error))
}

fn extract_native_encoded(
    identity: AuthorityIdentity,
    language: &str,
    snapshot: &DiscoverySnapshot,
    key: SessionKey,
    template: Option<&NativeTemplate>,
    request: &NativeRequestImage<'_>,
) -> Result<Extraction, AuthorityError> {
    if !key.matches(&identity, snapshot.manifest()) {
        return Err(AuthorityError::InvalidSessionKey);
    }

    let Some(template) = template else {
        return unsupported(identity, snapshot);
    };

    if !template.helper().exists() || !template.toolchain().exists() {
        return unavailable(identity, snapshot);
    }

    template.verify_toolchain()?;
    let request_bytes = request.bytes();
    let command = if template.persistent() {
        template.command_for(&identity, key, None)?
    } else {
        template.command_for(&identity, key, Some(request.stdin()))?
    };
    let registry = AuthorityRegistry::for_native(identity, snapshot, &command)
        .map_err(|error| AuthorityError::Extraction(error.to_string()))?;
    let runner = NativeAuthorityRunner::new(command);
    let observation = if template.persistent() {
        match runner.run_persistent_with_request(
            snapshot.manifest().digest(),
            snapshot.sequence(),
            request_bytes,
        ) {
            Ok(observation) => observation,
            Err(NativeRunnerError::Unavailable) => return unavailable(identity, snapshot),
            Err(NativeRunnerError::Unsupported) => return unsupported(identity, snapshot),
            Err(error) => return Err(map_runner_error(error)),
        }
    } else {
        match runner.run_cold() {
            Ok(observation) => observation,
            Err(NativeRunnerError::Unavailable) => return unavailable(identity, snapshot),
            Err(NativeRunnerError::Unsupported) => return unsupported(identity, snapshot),
            Err(error) => return Err(map_runner_error(error)),
        }
    };

    if template.persistent() {
        if observation.exit() != NativeExit::Running {
            return Err(AuthorityError::Extraction(
                "persistent native authority did not remain running".into(),
            ));
        }
    } else if !matches!(observation.exit(), NativeExit::Success(Some(0) | None)) {
        return Err(AuthorityError::Extraction(format!(
            "native authority exited unsuccessfully: {:?}",
            observation.exit()
        )));
    }

    // Persistent session framing owns stdout and the compile runner has
    // already strictly decoded the bounded stderr payload before returning.
    // Cold helpers carry the same envelope directly on stdout and are decoded
    // here. In neither mode can arbitrary compiler text become a fact.
    let envelope = if template.persistent() {
        observation.into_payload().ok_or_else(|| {
            AuthorityError::Extraction("persistent native authority omitted its payload".into())
        })?
    } else {
        NativeEnvelope::decode(observation.stdout()).map_err(|error| {
            AuthorityError::Extraction(format!("malformed native authority output: {error}"))
        })?
    };
    admit_envelope(&registry, language, key, envelope)
}

/// Returns a manifest-bound unavailable result.
///
/// # Errors
///
/// Returns [`AuthorityError`] when the unavailable result cannot be bound to
/// the supplied manifest and identity.
pub fn unavailable_extraction(
    identity: AuthorityIdentity,
    snapshot: &DiscoverySnapshot,
) -> Result<Extraction, AuthorityError> {
    unavailable(identity, snapshot)
}

fn unsupported(
    identity: AuthorityIdentity,
    snapshot: &DiscoverySnapshot,
) -> Result<Extraction, AuthorityError> {
    let scope = ScopeRoot::from_bytes(snapshot.manifest().digest().to_bytes());
    Extraction::bound_at(
        identity,
        snapshot.manifest(),
        snapshot.sequence(),
        CoverageWitness::Unsupported(UntrustedCoverageScope::from_scope_root(scope)),
        Vec::new(),
    )
    .map_err(|error| AuthorityError::Extraction(error.to_string()))
}

fn unavailable(
    identity: AuthorityIdentity,
    snapshot: &DiscoverySnapshot,
) -> Result<Extraction, AuthorityError> {
    let scope = ScopeRoot::from_bytes(snapshot.manifest().digest().to_bytes());
    Extraction::bound_at(
        identity,
        snapshot.manifest(),
        snapshot.sequence(),
        CoverageWitness::Unavailable(UntrustedCoverageScope::from_scope_root(scope)),
        Vec::new(),
    )
    .map_err(|error| AuthorityError::Extraction(error.to_string()))
}

fn map_runner_error(error: NativeRunnerError) -> AuthorityError {
    match error {
        NativeRunnerError::Unsupported => {
            AuthorityError::Extraction("native authority protocol operation is unsupported".into())
        }
        NativeRunnerError::Unavailable => {
            AuthorityError::Extraction("native authority is unavailable".into())
        }
        NativeRunnerError::Process(error) => AuthorityError::Process(error),
        NativeRunnerError::MissingToolchain => {
            AuthorityError::Extraction("native authority has no toolchain identity".into())
        }
        NativeRunnerError::MissingSessionKey => AuthorityError::InvalidSessionKey,
        NativeRunnerError::Protocol => {
            AuthorityError::Extraction("native authority session protocol failed".into())
        }
    }
}

fn map_preparation_error(error: &PreparationError) -> AuthorityError {
    AuthorityError::Extraction(error.to_string())
}

fn admit_envelope(
    registry: &AuthorityRegistry,
    language: &str,
    key: SessionKey,
    envelope: NativeEnvelope,
) -> Result<Extraction, AuthorityError> {
    let envelope = envelope
        .admit(key, registry.authority(), language, registry.revision())
        .map_err(|error| AuthorityError::Extraction(error.to_string()))?;
    let records = envelope
        .records()
        .iter()
        .map(record_to_fact)
        .collect::<Vec<_>>();
    match envelope.coverage() {
        NativeCoverage::Complete => registry.admit_native_records(key, &envelope, records),
        NativeCoverage::Partial => Extraction::bound_records(
            registry.authority(),
            registry.manifest_ref(),
            registry.revision(),
            partial_from_scope(registry.manifest()),
            records,
        )
        .map_err(|error| AuthorityError::Extraction(error.to_string())),
    }
}

fn partial_from_scope(manifest: InputManifestId) -> CoverageWitness {
    let scope = ScopeRoot::from_bytes(manifest.to_bytes());
    CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(scope))
}

fn record_to_fact(record: &crate::NativeRecord) -> FactRecord<FactKeySchema, FactValueSchema> {
    let facet = facet_for(record.kind());
    let kind = fact_kind_for(record.kind(), facet);
    // The semantic family is retained in `FactKind`; the value bytes stay
    // exactly as emitted so language adapters can materialize their own typed
    // facet value without reparsing an adapter-specific wrapper.
    FactRecord::new(
        kind,
        record.key().as_bytes().to_vec(),
        record.value().to_vec(),
    )
}

fn facet_for(kind: crate::NativeRecordKind) -> FacetKind {
    kind.semantic_facet()
}

fn fact_kind_for(native: crate::NativeRecordKind, facet: FacetKind) -> FactKind {
    match native {
        crate::NativeRecordKind::Declaration if facet == FacetKind::Entity => FactKind::Declaration,
        crate::NativeRecordKind::Type if facet == FacetKind::Type => FactKind::Type,
        crate::NativeRecordKind::Edge if facet == FacetKind::Edge => FactKind::Edge,
        crate::NativeRecordKind::Diagnostic if facet == FacetKind::Facet => FactKind::Diagnostic,
        crate::NativeRecordKind::Dependency if facet == FacetKind::Configuration => {
            FactKind::Dependency
        }
        crate::NativeRecordKind::NegativeDependency if facet == FacetKind::Configuration => {
            FactKind::NegativeDependency
        }
        _ => FactKind::Diagnostic,
    }
}
