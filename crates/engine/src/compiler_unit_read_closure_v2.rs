//! Internal construction of complete, target-bound compiler read closures.
//!
//! A full-workspace snapshot is not evidence that a compiler read every input
//! it used. This module keeps the stronger value behind an internal trace
//! builder: only a reviewed adapter protocol can start it, every event must be
//! sequenced, and the adapter must provide an end marker before the workspace
//! witnesses are checked. No wire or JSON completeness flag is accepted here.

use crate::compiler_input_manifest_v2::{
    CompilerInputManifestV2, CompilerInvocationRecipeV2, CompilerPackageTargetV2,
    CompilerWorkspaceSnapshotIdV2,
};
use crate::compiler_input_tree_v2::{
    CompilerInputMerkleTreeV2, CompilerInputTreeKindV2, CompilerInputTreeRecordV2,
    CompilerInputTreeV2Error, MAX_TREE_RECORD_CHARGE_BYTES, MAX_TREE_RECORDS, compare_records,
};
use backend_semantic::vocabulary::{Language, LanguageProfile, Stage};
use backend_version::CompilationTargetDomain;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

const ADAPTER_PROTOCOL_DOMAIN: &[u8] = b"backend.compiler.read-adapter-protocol.v2\0";
const UNIT_READ_CLOSURE_DOMAIN: &[u8] = b"backend.compiler.verified-unit-read-closure.v2\0";
const PURE_UNIT_KEY_DOMAIN: &[u8] = b"backend.compiler.pure-unit-key.v2\0";
const MAX_ADAPTER_NAME_BYTES: usize = 128;
const MAX_READ_OBSERVATION_BYTES: u64 = 512 * 1024 * 1024;
const READ_OBSERVATION_DOMAIN: &[u8] = b"backend.compiler.read-observation.v2\0";
static NEXT_READ_ATTEMPT_ID: AtomicU64 = AtomicU64::new(1);
// Kept empty until an in-process adapter protocol has reviewed, demonstrated
// full read coverage, and received a dedicated registration here.
const TRUSTED_ADAPTER_PROTOCOLS_V2: &[(&str, u32, [u8; 32])] = &[];

/// Stable identity of the reviewed protocol used to collect compiler reads.
///
/// The identity commits to protocol name, revision, and protocol specification
/// digest. It deliberately excludes host paths and machine fingerprints.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CompilerReadAdapterProtocolIdentityV2([u8; 32]);

impl CompilerReadAdapterProtocolIdentityV2 {
    /// Exact stable identity bytes for this trusted adapter protocol.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Copies the exact stable adapter protocol identity bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Domain-separated content key for one target, portable recipe, and complete read closure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PureUnitKey([u8; 32]);

impl PureUnitKey {
    /// Returns the fixed-width key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Copies the fixed-width key bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Why an instrumented compiler read trace could not become a complete closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerReadClosureIncompleteReasonV2 {
    /// The trusted adapter did not emit the required trace start marker.
    MissingInstrumentation,
    /// The trusted adapter did not close the trace with its final event count.
    MissingEndMarker,
    /// One or more events were dropped or their sequence was discontinuous.
    TruncatedOrDiscontinuousTrace,
    /// The trace was used out of order or after it had already been closed.
    InvalidTraceOrder,
    /// Adapter protocol metadata or an event did not meet the closed schema.
    InvalidAdapterProtocol,
    /// The adapter protocol has not been admitted by the internal trust registry.
    UnregisteredAdapterProtocol,
    /// An event did not describe a supported read fact.
    InvalidReadFact,
    /// The bounded event count or charged fact bytes were exceeded.
    Limit,
}

/// Independent observation channels required before a future Rust adapter
/// could attempt to establish a complete compiler read frontier.
///
/// These channels are intentionally more granular than the current RA VFS
/// callback. Sealing them proves only that registered producers closed their
/// event streams; it does not prove that a producer observed every read.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub(crate) enum CompilerReadObservationChannelV2 {
    /// Exact selected editor buffers installed in RA's database.
    EditorOverlay = 0,
    /// Positive file buffers delivered through the RA VFS loader.
    RaVfsLoader = 1,
    /// Positive and negative Rust module-resolution candidates.
    RustModuleResolver = 2,
    /// Other authority filesystem probes, including failed metadata/open calls.
    AuthorityFilesystem = 3,
    /// Complete direct-child sets and errors from directory enumeration.
    DirectoryEnumeration = 4,
    /// Cargo manifest discovery, metadata, config, and project-model reads.
    CargoProjectModel = 5,
    /// Present and absent environment values consulted by the authority.
    Environment = 6,
    /// Executable identity, arguments, output, and lifecycle for every child.
    ProcessTree = 7,
    /// Rust toolchain, rustup, sysroot, and standard-library inputs.
    ToolchainSysroot = 8,
    /// Build-script or other generated bytes consumed by the authority.
    GeneratedOutputs = 9,
    /// Registry, path dependency, and other external source inputs.
    ExternalDependencies = 10,
}

impl CompilerReadObservationChannelV2 {
    const ALL: [Self; 11] = [
        Self::EditorOverlay,
        Self::RaVfsLoader,
        Self::RustModuleResolver,
        Self::AuthorityFilesystem,
        Self::DirectoryEnumeration,
        Self::CargoProjectModel,
        Self::Environment,
        Self::ProcessTree,
        Self::ToolchainSysroot,
        Self::GeneratedOutputs,
        Self::ExternalDependencies,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    const fn label(self) -> &'static str {
        match self {
            Self::EditorOverlay => "editor_overlay",
            Self::RaVfsLoader => "ra_vfs_loader",
            Self::RustModuleResolver => "rust_module_resolver",
            Self::AuthorityFilesystem => "authority_filesystem",
            Self::DirectoryEnumeration => "directory_enumeration",
            Self::CargoProjectModel => "cargo_project_model",
            Self::Environment => "environment",
            Self::ProcessTree => "process_tree",
            Self::ToolchainSysroot => "toolchain_sysroot",
            Self::GeneratedOutputs => "generated_outputs",
            Self::ExternalDependencies => "external_dependencies",
        }
    }
}

/// Closed event classes accepted by the attempt-scoped diagnostic recorder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum CompilerReadObservationEventClassV2 {
    /// Exact selected editor-overlay bytes.
    EditorBuffer = 1,
    /// A positive filesystem or VFS read.
    PresentFile = 2,
    /// A failed lookup whose absence can affect compilation.
    AbsentPath = 3,
    /// A complete directory enumeration result.
    DirectoryListing = 4,
    /// A present or absent environment-variable observation.
    EnvironmentValue = 5,
    /// A Cargo/project-model observation.
    CargoProject = 6,
    /// One child process identity, output, or lifecycle event.
    ChildProcess = 7,
    /// A toolchain or sysroot input.
    ToolchainInput = 8,
    /// A generated output input.
    GeneratedOutput = 9,
    /// An external dependency input.
    ExternalInput = 10,
    /// A Rust module-resolution candidate or membership result.
    ModuleResolution = 11,
    /// The producer encountered a read class outside its declared contract.
    Unsupported = 255,
}

/// Why a diagnostic observation attempt could not seal its producer set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompilerReadObservationFailureV2 {
    /// Attempt identifiers are exhausted.
    AttemptIdExhausted,
    /// A producer was registered more than once.
    DuplicateProducer,
    /// A token was forged, stale, or belongs to another attempt.
    StaleAttemptOrProducer,
    /// The producer sequence was not contiguous.
    SequenceGap,
    /// The producer's independent terminal count disagreed with delivered events.
    FinalCountMismatch,
    /// The event class did not belong to its registered channel.
    UnsupportedReadClass,
    /// The bounded event or byte budget was exceeded.
    Limit,
    /// A path supplied to a path-bearing event was not portable and canonical.
    UnsupportedPath,
}

/// Attempt-local token issued only by [`CompilerReadObservationRecorderV2`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct CompilerReadObservationProducerV2 {
    attempt_id: u64,
    channel: CompilerReadObservationChannelV2,
}

struct CompilerReadObservationChannelStateV2 {
    registered: bool,
    sealed: bool,
    next_sequence: u64,
    transcript: blake3::Hasher,
}

impl CompilerReadObservationChannelStateV2 {
    fn new(attempt_id: u64, channel: CompilerReadObservationChannelV2) -> Self {
        let mut transcript = blake3::Hasher::new();
        transcript.update(READ_OBSERVATION_DOMAIN);
        transcript.update(&attempt_id.to_be_bytes());
        transcript.update(&[channel as u8]);
        Self {
            registered: false,
            sealed: false,
            next_sequence: 0,
            transcript,
        }
    }
}

/// Single-owner, bounded event broker for one diagnostic compiler attempt.
///
/// It retains only per-channel rolling transcripts and counters, not an event
/// allocation per read. Producers are registered and drained synchronously by
/// the owning compiler lane. A future child-process broker may merge events
/// only after child termination and must seal with an independently obtained
/// final count. This recorder never mints `VerifiedUnitReadClosure` and is not
/// a trust-registry entry.
pub(crate) struct CompilerReadObservationRecorderV2 {
    attempt_id: u64,
    channels: [CompilerReadObservationChannelStateV2; 11],
    failure: Option<CompilerReadObservationFailureV2>,
    event_count: u64,
    byte_count: u64,
}

/// Bounded administrative report about producer registration and event delivery.
///
/// `all_required_producers_sealed` means only that each known producer token
/// reported a terminal count. It is not a statement that compiler reads were
/// completely observed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompilerReadObservationReportV2 {
    attempt_id: u64,
    registered_channels: usize,
    sealed_channels: usize,
    required_channels: usize,
    events: u64,
    bytes: u64,
    failure: Option<CompilerReadObservationFailureV2>,
    missing_channels: Vec<CompilerReadObservationChannelV2>,
    unsealed_channels: Vec<CompilerReadObservationChannelV2>,
    transcript_digest: [u8; 32],
}

impl CompilerReadObservationReportV2 {
    pub(crate) const fn attempt_id(&self) -> u64 {
        self.attempt_id
    }

    pub(crate) const fn registered_channels(&self) -> usize {
        self.registered_channels
    }

    pub(crate) const fn sealed_channels(&self) -> usize {
        self.sealed_channels
    }

    pub(crate) const fn required_channels(&self) -> usize {
        self.required_channels
    }

    pub(crate) const fn events(&self) -> u64 {
        self.events
    }

    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) const fn failure(&self) -> Option<CompilerReadObservationFailureV2> {
        self.failure
    }

    pub(crate) const fn transcript_digest(&self) -> [u8; 32] {
        self.transcript_digest
    }

    pub(crate) fn all_required_producers_sealed(&self) -> bool {
        self.failure.is_none()
            && self.registered_channels == self.required_channels
            && self.sealed_channels == self.required_channels
            && self.missing_channels.is_empty()
            && self.unsealed_channels.is_empty()
    }

    pub(crate) fn missing_channel_labels(&self) -> Vec<&'static str> {
        self.missing_channels
            .iter()
            .chain(&self.unsealed_channels)
            .copied()
            .map(CompilerReadObservationChannelV2::label)
            .collect()
    }
}

impl CompilerReadObservationRecorderV2 {
    pub(crate) fn new() -> Result<Self, CompilerReadObservationFailureV2> {
        let attempt_id = NEXT_READ_ATTEMPT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| CompilerReadObservationFailureV2::AttemptIdExhausted)?;
        Ok(Self {
            attempt_id,
            channels: std::array::from_fn(|index| {
                CompilerReadObservationChannelStateV2::new(
                    attempt_id,
                    CompilerReadObservationChannelV2::ALL[index],
                )
            }),
            failure: None,
            event_count: 0,
            byte_count: 0,
        })
    }

    pub(crate) fn register(
        &mut self,
        channel: CompilerReadObservationChannelV2,
    ) -> Result<CompilerReadObservationProducerV2, CompilerReadObservationFailureV2> {
        self.check_healthy()?;
        let state = &mut self.channels[channel.index()];
        if state.registered {
            return self.poison(CompilerReadObservationFailureV2::DuplicateProducer);
        }
        state.registered = true;
        Ok(CompilerReadObservationProducerV2 {
            attempt_id: self.attempt_id,
            channel,
        })
    }

    pub(crate) fn observe_editor_buffer(
        &mut self,
        producer: &CompilerReadObservationProducerV2,
        sequence: u64,
        relative_path: &str,
        contents: &[u8],
    ) -> Result<(), CompilerReadObservationFailureV2> {
        if producer.channel != CompilerReadObservationChannelV2::EditorOverlay {
            return self.poison(CompilerReadObservationFailureV2::UnsupportedReadClass);
        }
        if relative_path.is_empty()
            || crate::compiler_input_manifest_v2::validate_compiler_input_path_v2(relative_path)
                .is_err()
        {
            return self.poison(CompilerReadObservationFailureV2::UnsupportedPath);
        }
        self.check_healthy()?;
        if producer.attempt_id != self.attempt_id
            || producer.channel != CompilerReadObservationChannelV2::EditorOverlay
        {
            return self.poison(CompilerReadObservationFailureV2::StaleAttemptOrProducer);
        }
        let state = &self.channels[producer.channel.index()];
        if !state.registered || state.sealed {
            return self.poison(CompilerReadObservationFailureV2::StaleAttemptOrProducer);
        }
        if sequence != state.next_sequence {
            return self.poison(CompilerReadObservationFailureV2::SequenceGap);
        }
        if self.event_count >= MAX_TREE_RECORDS as u64 {
            return self.poison(CompilerReadObservationFailureV2::Limit);
        }
        let byte_charge = u64::try_from(contents.len()).unwrap_or(u64::MAX);
        if self
            .byte_count
            .checked_add(byte_charge)
            .is_none_or(|total_bytes| total_bytes > MAX_READ_OBSERVATION_BYTES)
        {
            return self.poison(CompilerReadObservationFailureV2::Limit);
        }
        let content_digest = blake3::hash(contents);
        let mut identity = blake3::Hasher::new();
        identity.update(b"backend.compiler.editor-buffer-observation.v2\0");
        identity.update(&(relative_path.len() as u32).to_be_bytes());
        identity.update(relative_path.as_bytes());
        identity.update(&byte_charge.to_be_bytes());
        identity.update(content_digest.as_bytes());
        self.observe_digest(
            producer,
            sequence,
            CompilerReadObservationEventClassV2::EditorBuffer,
            *identity.finalize().as_bytes(),
            byte_charge,
        )
    }

    /// Records one canonical path-bearing observation. The path is included
    /// in the transcript identity, and unsupported spellings poison the
    /// attempt before it can be sealed.
    pub(crate) fn observe_path_event(
        &mut self,
        producer: &CompilerReadObservationProducerV2,
        sequence: u64,
        class: CompilerReadObservationEventClassV2,
        path: &str,
        evidence_digest: [u8; 32],
        byte_charge: u64,
    ) -> Result<(), CompilerReadObservationFailureV2> {
        if !matches!(
            class,
            CompilerReadObservationEventClassV2::PresentFile
                | CompilerReadObservationEventClassV2::AbsentPath
                | CompilerReadObservationEventClassV2::DirectoryListing
                | CompilerReadObservationEventClassV2::ModuleResolution
                | CompilerReadObservationEventClassV2::ToolchainInput
                | CompilerReadObservationEventClassV2::GeneratedOutput
                | CompilerReadObservationEventClassV2::ExternalInput
        ) {
            return self.poison(CompilerReadObservationFailureV2::UnsupportedReadClass);
        }
        if path.is_empty()
            || crate::compiler_input_manifest_v2::validate_compiler_input_path_v2(path).is_err()
        {
            return self.poison(CompilerReadObservationFailureV2::UnsupportedPath);
        }
        if evidence_digest == [0; 32] {
            return self.poison(CompilerReadObservationFailureV2::UnsupportedReadClass);
        }
        let mut identity = blake3::Hasher::new();
        identity.update(b"backend.compiler.path-observation.v2\0");
        identity.update(&[class as u8]);
        identity.update(&(path.len() as u32).to_be_bytes());
        identity.update(path.as_bytes());
        identity.update(&evidence_digest);
        self.observe_digest(
            producer,
            sequence,
            class,
            *identity.finalize().as_bytes(),
            byte_charge,
        )
    }

    /// Records an environment, project-model, or process event whose exact
    /// identity has already been encoded by its channel adapter.
    pub(crate) fn observe_nonpath_event(
        &mut self,
        producer: &CompilerReadObservationProducerV2,
        sequence: u64,
        class: CompilerReadObservationEventClassV2,
        evidence_digest: [u8; 32],
        byte_charge: u64,
    ) -> Result<(), CompilerReadObservationFailureV2> {
        if !matches!(
            class,
            CompilerReadObservationEventClassV2::EnvironmentValue
                | CompilerReadObservationEventClassV2::CargoProject
                | CompilerReadObservationEventClassV2::ChildProcess
        ) || evidence_digest == [0; 32]
        {
            return self.poison(CompilerReadObservationFailureV2::UnsupportedReadClass);
        }
        let mut identity = blake3::Hasher::new();
        identity.update(b"backend.compiler.nonpath-observation.v2\0");
        identity.update(&[class as u8]);
        identity.update(&evidence_digest);
        self.observe_digest(
            producer,
            sequence,
            class,
            *identity.finalize().as_bytes(),
            byte_charge,
        )
    }

    fn observe_digest(
        &mut self,
        producer: &CompilerReadObservationProducerV2,
        sequence: u64,
        class: CompilerReadObservationEventClassV2,
        identity: [u8; 32],
        byte_charge: u64,
    ) -> Result<(), CompilerReadObservationFailureV2> {
        self.check_healthy()?;
        if producer.attempt_id != self.attempt_id {
            return self.poison(CompilerReadObservationFailureV2::StaleAttemptOrProducer);
        }
        if !class_matches_channel(class, producer.channel) {
            return self.poison(CompilerReadObservationFailureV2::UnsupportedReadClass);
        }
        if class == CompilerReadObservationEventClassV2::Unsupported || identity == [0; 32] {
            return self.poison(CompilerReadObservationFailureV2::UnsupportedReadClass);
        }
        let state = &mut self.channels[producer.channel.index()];
        if !state.registered || state.sealed {
            return self.poison(CompilerReadObservationFailureV2::StaleAttemptOrProducer);
        }
        if sequence != state.next_sequence {
            return self.poison(CompilerReadObservationFailureV2::SequenceGap);
        }
        if self.event_count >= MAX_TREE_RECORDS as u64 {
            return self.poison(CompilerReadObservationFailureV2::Limit);
        }
        let Some(total_bytes) = self.byte_count.checked_add(byte_charge) else {
            return self.poison(CompilerReadObservationFailureV2::Limit);
        };
        if total_bytes > MAX_READ_OBSERVATION_BYTES {
            return self.poison(CompilerReadObservationFailureV2::Limit);
        }
        let Some(next_sequence) = state.next_sequence.checked_add(1) else {
            return self.poison(CompilerReadObservationFailureV2::Limit);
        };
        state.transcript.update(&sequence.to_be_bytes());
        state.transcript.update(&[class as u8]);
        state.transcript.update(&identity);
        state.transcript.update(&byte_charge.to_be_bytes());
        state.next_sequence = next_sequence;
        self.event_count = self.event_count.saturating_add(1);
        self.byte_count = total_bytes;
        Ok(())
    }

    pub(crate) fn seal(
        &mut self,
        producer: &CompilerReadObservationProducerV2,
        final_event_count: u64,
    ) -> Result<(), CompilerReadObservationFailureV2> {
        self.check_healthy()?;
        if producer.attempt_id != self.attempt_id {
            return self.poison(CompilerReadObservationFailureV2::StaleAttemptOrProducer);
        }
        let state = &mut self.channels[producer.channel.index()];
        if !state.registered || state.sealed {
            return self.poison(CompilerReadObservationFailureV2::StaleAttemptOrProducer);
        }
        if final_event_count != state.next_sequence {
            return self.poison(CompilerReadObservationFailureV2::FinalCountMismatch);
        }
        state.sealed = true;
        Ok(())
    }

    pub(crate) fn report(&self) -> CompilerReadObservationReportV2 {
        let mut missing_channels = Vec::new();
        let mut unsealed_channels = Vec::new();
        let mut registered_channels = 0;
        let mut sealed_channels = 0;
        let mut transcript = blake3::Hasher::new();
        transcript.update(READ_OBSERVATION_DOMAIN);
        transcript.update(&self.attempt_id.to_be_bytes());
        for channel in CompilerReadObservationChannelV2::ALL {
            let state = &self.channels[channel.index()];
            transcript.update(&[channel as u8]);
            transcript.update(&[u8::from(state.registered), u8::from(state.sealed)]);
            transcript.update(&state.next_sequence.to_be_bytes());
            transcript.update(state.transcript.clone().finalize().as_bytes());
            if !state.registered {
                missing_channels.push(channel);
            } else {
                registered_channels += 1;
                if state.sealed {
                    sealed_channels += 1;
                } else {
                    unsealed_channels.push(channel);
                }
            }
        }
        CompilerReadObservationReportV2 {
            attempt_id: self.attempt_id,
            registered_channels,
            sealed_channels,
            required_channels: CompilerReadObservationChannelV2::ALL.len(),
            events: self.event_count,
            bytes: self.byte_count,
            failure: self.failure,
            missing_channels,
            unsealed_channels,
            transcript_digest: *transcript.finalize().as_bytes(),
        }
    }

    #[cfg(test)]
    fn channel_event_count(&self, channel: CompilerReadObservationChannelV2) -> u64 {
        self.channels[channel.index()].next_sequence
    }

    fn check_healthy(&self) -> Result<(), CompilerReadObservationFailureV2> {
        self.failure.map_or(Ok(()), Err)
    }

    fn poison<T>(
        &mut self,
        failure: CompilerReadObservationFailureV2,
    ) -> Result<T, CompilerReadObservationFailureV2> {
        self.failure = Some(failure);
        Err(failure)
    }
}

fn class_matches_channel(
    class: CompilerReadObservationEventClassV2,
    channel: CompilerReadObservationChannelV2,
) -> bool {
    use CompilerReadObservationChannelV2 as Channel;
    use CompilerReadObservationEventClassV2 as Class;
    match channel {
        Channel::EditorOverlay => class == Class::EditorBuffer,
        Channel::RaVfsLoader | Channel::AuthorityFilesystem => {
            matches!(class, Class::PresentFile | Class::AbsentPath)
        }
        Channel::RustModuleResolver => matches!(
            class,
            Class::PresentFile | Class::AbsentPath | Class::ModuleResolution
        ),
        Channel::DirectoryEnumeration => class == Class::DirectoryListing,
        Channel::CargoProjectModel => {
            matches!(
                class,
                Class::CargoProject
                    | Class::PresentFile
                    | Class::AbsentPath
                    | Class::DirectoryListing
            )
        }
        Channel::Environment => class == Class::EnvironmentValue,
        Channel::ProcessTree => class == Class::ChildProcess,
        Channel::ToolchainSysroot => matches!(
            class,
            Class::ToolchainInput
                | Class::PresentFile
                | Class::AbsentPath
                | Class::DirectoryListing
                | Class::ChildProcess
        ),
        Channel::GeneratedOutputs => class == Class::GeneratedOutput,
        Channel::ExternalDependencies => {
            matches!(class, Class::ExternalInput | Class::AbsentPath)
        }
    }
}

/// Rejection while constructing or checking a target-bound unit read closure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CompilerUnitReadClosureErrorV2 {
    /// The trace lacked trusted, complete instrumentation evidence.
    #[error("compiler read trace is incomplete: {0:?}")]
    Incomplete(CompilerReadClosureIncompleteReasonV2),
    /// The captured root identity did not match the supplied workspace tree.
    #[error("compiler read trace is bound to another captured workspace root")]
    WorkspaceIdentityMismatch,
    /// The native unit kind and portable recipe language profile disagree.
    #[error("compiler unit target and portable recipe use different language profiles")]
    TargetProfileMismatch,
    /// A positive, negative, or directory-listing witness disagreed with the workspace.
    #[error("compiler read facts conflict with the captured workspace")]
    WorkspaceReadMismatch,
    /// The canonical fact tree could not be built or independently verified.
    #[error(transparent)]
    Tree(#[from] CompilerInputTreeV2Error),
}

/// Exact workspace capture identity carried by a verified closure.
///
/// This is proof-binding metadata. The pure unit key uses only the fact root,
/// target, and portable invocation recipe, so checkout identity and source
/// provenance do not partition equivalent compiler work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerWorkspaceCaptureIdentityV2 {
    workspace_snapshot_id: CompilerWorkspaceSnapshotIdV2,
    workspace_root: [u8; 32],
}

impl CompilerWorkspaceCaptureIdentityV2 {
    fn from_manifest(manifest: &CompilerInputManifestV2) -> Self {
        Self {
            workspace_snapshot_id: manifest.workspace_snapshot_id(),
            workspace_root: manifest.workspace_root(),
        }
    }

    /// Exact typed identity of the admitted workspace snapshot.
    #[must_use]
    pub const fn workspace_snapshot_id(self) -> CompilerWorkspaceSnapshotIdV2 {
        self.workspace_snapshot_id
    }

    /// Root identity of the captured workspace Merkle tree.
    #[must_use]
    pub const fn workspace_root(self) -> [u8; 32] {
        self.workspace_root
    }
}

/// Counters recorded while verifying the complete trace and workspace witnesses.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompilerUnitReadClosureStatsV2 {
    /// Number of read events emitted by the trusted adapter, including repeats.
    pub trace_events: usize,
    /// Number of distinct canonical facts retained in the closure tree.
    pub unique_facts: usize,
    /// Number of pages in the canonical read-fact Merkle tree.
    pub fact_pages: usize,
    /// Encoded bytes in the canonical read-fact Merkle tree.
    pub fact_encoded_bytes: usize,
    /// Workspace pages scanned to verify directory listings; zero when none were observed.
    pub workspace_pages_scanned: usize,
}

/// Verified complete read closure for one exact package compilation unit.
///
/// Fields are private and there is no public constructor or deserializer. A
/// value is created only by `CompilerReadTraceBuilderV2::finish` after a
/// reviewed adapter protocol has produced a closed, sequenced trace and all
/// workspace-relative witnesses have been checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedUnitReadClosure {
    package_target: CompilerPackageTargetV2,
    invocation_recipe: CompilerInvocationRecipeV2,
    capture_identity: CompilerWorkspaceCaptureIdentityV2,
    adapter_protocol: CompilerReadAdapterProtocolIdentityV2,
    fact_tree: CompilerInputMerkleTreeV2,
    closure_root: [u8; 32],
    stats: CompilerUnitReadClosureStatsV2,
}

impl VerifiedUnitReadClosure {
    /// Exact package and language-native unit this closure proves.
    #[must_use]
    pub const fn package_target(&self) -> &CompilerPackageTargetV2 {
        &self.package_target
    }

    /// Portable compiler invocation recipe bound to this closure.
    #[must_use]
    pub const fn invocation_recipe(&self) -> CompilerInvocationRecipeV2 {
        self.invocation_recipe
    }

    /// Language profile derived from the bound portable recipe.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.invocation_recipe.profile()
    }

    /// Compiler stage derived from the bound portable recipe.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.invocation_recipe.stage()
    }

    /// Exact workspace capture identity checked during closure verification.
    #[must_use]
    pub const fn capture_identity(&self) -> CompilerWorkspaceCaptureIdentityV2 {
        self.capture_identity
    }

    /// Stable trusted adapter protocol identity used to collect the reads.
    #[must_use]
    pub const fn adapter_protocol(&self) -> CompilerReadAdapterProtocolIdentityV2 {
        self.adapter_protocol
    }

    /// Root committing to every canonical read fact and adapter protocol.
    #[must_use]
    pub const fn closure_root(&self) -> [u8; 32] {
        self.closure_root
    }

    /// Borrowed canonical fact tree retained by this proof.
    #[must_use]
    pub const fn fact_tree(&self) -> &CompilerInputMerkleTreeV2 {
        &self.fact_tree
    }

    /// Construction and verification counters for diagnostics and benchmarks.
    #[must_use]
    pub const fn stats(&self) -> CompilerUnitReadClosureStatsV2 {
        self.stats
    }

    /// Pure content identity of the target, portable recipe, and complete read closure.
    #[must_use]
    pub fn pure_unit_key(&self) -> PureUnitKey {
        derive_pure_unit_key(
            self.package_target.target(),
            self.invocation_recipe,
            self.closure_root,
        )
    }
}

/// Internal capability minted only for a reviewed compiler read-adapter protocol.
///
/// There is deliberately no public constructor. No production language lane
/// currently mints this capability or a complete read closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TrustedCompilerReadAdapterProtocolV2 {
    identity: CompilerReadAdapterProtocolIdentityV2,
}

impl TrustedCompilerReadAdapterProtocolV2 {
    /// Internal registry path for protocols whose read coverage was reviewed.
    #[allow(dead_code)]
    pub(crate) fn registered(
        name: &str,
        revision: u32,
        protocol_spec_digest: [u8; 32],
    ) -> Result<Self, CompilerUnitReadClosureErrorV2> {
        let name_is_canonical = !name.is_empty()
            && name.len() <= MAX_ADAPTER_NAME_BYTES
            && name.is_ascii()
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            });
        if !name_is_canonical || revision == 0 || protocol_spec_digest == [0; 32] {
            return Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::InvalidAdapterProtocol,
            ));
        }
        if !TRUSTED_ADAPTER_PROTOCOLS_V2
            .iter()
            .any(|entry| *entry == (name, revision, protocol_spec_digest))
        {
            return Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::UnregisteredAdapterProtocol,
            ));
        }
        Ok(Self {
            identity: derive_adapter_protocol_identity(name, revision, protocol_spec_digest),
        })
    }

    #[cfg(test)]
    fn fixture(
        name: &str,
        revision: u32,
        protocol_spec_digest: [u8; 32],
    ) -> Result<Self, CompilerUnitReadClosureErrorV2> {
        let name_is_canonical = !name.is_empty()
            && name.len() <= MAX_ADAPTER_NAME_BYTES
            && name.is_ascii()
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            });
        if !name_is_canonical || revision == 0 || protocol_spec_digest == [0; 32] {
            return Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::InvalidAdapterProtocol,
            ));
        }
        Ok(Self {
            identity: derive_adapter_protocol_identity(name, revision, protocol_spec_digest),
        })
    }
}

fn derive_adapter_protocol_identity(
    name: &str,
    revision: u32,
    protocol_spec_digest: [u8; 32],
) -> CompilerReadAdapterProtocolIdentityV2 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ADAPTER_PROTOCOL_DOMAIN);
    hasher.update(&(name.len() as u32).to_be_bytes());
    hasher.update(name.as_bytes());
    hasher.update(&revision.to_be_bytes());
    hasher.update(&protocol_spec_digest);
    CompilerReadAdapterProtocolIdentityV2(*hasher.finalize().as_bytes())
}

/// Bounded internal event collector for a trusted compiler read adapter.
///
/// The API has explicit begin, ordered event, end, and truncation operations;
/// it accepts no `complete: bool` field and has no serde implementation.
#[allow(dead_code)]
pub(crate) struct CompilerReadTraceBuilderV2 {
    protocol: TrustedCompilerReadAdapterProtocolV2,
    package_target: CompilerPackageTargetV2,
    invocation_recipe: CompilerInvocationRecipeV2,
    capture_identity: CompilerWorkspaceCaptureIdentityV2,
    started: bool,
    ended: bool,
    failure: Option<CompilerReadClosureIncompleteReasonV2>,
    next_sequence: u64,
    events: usize,
    charged_fact_bytes: usize,
    facts: Vec<CompilerInputTreeRecordV2>,
}

#[allow(dead_code)]
impl CompilerReadTraceBuilderV2 {
    /// Opens a trace for the exact target, recipe, and captured workspace in a manifest.
    pub(crate) fn for_manifest(
        protocol: TrustedCompilerReadAdapterProtocolV2,
        manifest: &CompilerInputManifestV2,
    ) -> Self {
        Self {
            protocol,
            package_target: manifest.package_target().clone(),
            invocation_recipe: manifest.invocation_recipe(),
            capture_identity: CompilerWorkspaceCaptureIdentityV2::from_manifest(manifest),
            started: false,
            ended: false,
            failure: None,
            next_sequence: 0,
            events: 0,
            charged_fact_bytes: 0,
            facts: Vec::new(),
        }
    }

    /// Records the adapter's trace-start marker.
    pub(crate) fn begin(&mut self) -> Result<(), CompilerUnitReadClosureErrorV2> {
        if self.started || self.ended {
            return self.poison(CompilerReadClosureIncompleteReasonV2::InvalidTraceOrder);
        }
        self.started = true;
        Ok(())
    }

    /// Adds one exact event with its adapter-assigned sequence number.
    pub(crate) fn observe(
        &mut self,
        sequence: u64,
        fact: CompilerInputTreeRecordV2,
    ) -> Result<(), CompilerUnitReadClosureErrorV2> {
        self.check_healthy()?;
        if !self.started || self.ended {
            return self.poison(CompilerReadClosureIncompleteReasonV2::InvalidTraceOrder);
        }
        if sequence != self.next_sequence {
            return self
                .poison(CompilerReadClosureIncompleteReasonV2::TruncatedOrDiscontinuousTrace);
        }
        if !fact.is_read_record() {
            return self.poison(CompilerReadClosureIncompleteReasonV2::InvalidReadFact);
        }
        if self.events >= MAX_TREE_RECORDS {
            return self.poison(CompilerReadClosureIncompleteReasonV2::Limit);
        }
        let charge = fact
            .path()
            .len()
            .checked_mul(3)
            .and_then(|bytes| bytes.checked_add(256));
        let Some(charge) = charge else {
            return self.poison(CompilerReadClosureIncompleteReasonV2::Limit);
        };
        let Some(charged_fact_bytes) = self.charged_fact_bytes.checked_add(charge) else {
            return self.poison(CompilerReadClosureIncompleteReasonV2::Limit);
        };
        self.charged_fact_bytes = charged_fact_bytes;
        if self.charged_fact_bytes > MAX_TREE_RECORD_CHARGE_BYTES {
            return self.poison(CompilerReadClosureIncompleteReasonV2::Limit);
        }
        if self.facts.try_reserve(1).is_err() {
            return self.poison(CompilerReadClosureIncompleteReasonV2::Limit);
        }
        self.facts.push(fact);
        self.events += 1;
        let Some(next_sequence) = self.next_sequence.checked_add(1) else {
            return self.poison(CompilerReadClosureIncompleteReasonV2::Limit);
        };
        self.next_sequence = next_sequence;
        Ok(())
    }

    /// Marks the trace incomplete after any instrumentation or event-buffer loss.
    pub(crate) fn mark_truncated(&mut self) {
        self.failure = Some(CompilerReadClosureIncompleteReasonV2::TruncatedOrDiscontinuousTrace);
    }

    /// Records the adapter's final event count and closes the trace.
    pub(crate) fn end(
        &mut self,
        final_event_count: u64,
    ) -> Result<(), CompilerUnitReadClosureErrorV2> {
        self.check_healthy()?;
        if !self.started || self.ended {
            return self.poison(CompilerReadClosureIncompleteReasonV2::InvalidTraceOrder);
        }
        if final_event_count != self.next_sequence {
            return self
                .poison(CompilerReadClosureIncompleteReasonV2::TruncatedOrDiscontinuousTrace);
        }
        self.ended = true;
        Ok(())
    }

    /// Seals and independently checks the trace against its exact captured workspace tree.
    pub(crate) fn finish(
        mut self,
        workspace: &CompilerInputMerkleTreeV2,
    ) -> Result<VerifiedUnitReadClosure, CompilerUnitReadClosureErrorV2> {
        self.check_healthy()?;
        if !self.started {
            return Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::MissingInstrumentation,
            ));
        }
        if !self.ended {
            return Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::MissingEndMarker,
            ));
        }
        if !target_matches_profile(&self.package_target, self.invocation_recipe.profile()) {
            return Err(CompilerUnitReadClosureErrorV2::TargetProfileMismatch);
        }
        if workspace.kind() != CompilerInputTreeKindV2::Workspace
            || workspace.root() != self.capture_identity.workspace_root
        {
            return Err(CompilerUnitReadClosureErrorV2::WorkspaceIdentityMismatch);
        }
        if self.events == 0 {
            return Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::MissingInstrumentation,
            ));
        }

        self.facts.sort_by(compare_records);
        let mut conflicting_duplicate = false;
        self.facts.dedup_by(|later, earlier| {
            if compare_records(later, earlier).is_eq() {
                if later != earlier {
                    conflicting_duplicate = true;
                }
                true
            } else {
                false
            }
        });
        if conflicting_duplicate {
            return Err(CompilerUnitReadClosureErrorV2::WorkspaceReadMismatch);
        }
        let event_count = self.events;
        let fact_tree = CompilerInputMerkleTreeV2::from_sorted_records(
            CompilerInputTreeKindV2::ReadFrontier,
            self.facts,
        )?;
        let workspace_pages_scanned = fact_tree
            .verify_read_frontier_with_work(workspace)
            .map_err(|_| CompilerUnitReadClosureErrorV2::WorkspaceReadMismatch)?;
        let closure_root = derive_closure_root(self.protocol.identity, fact_tree.root());
        let stats = CompilerUnitReadClosureStatsV2 {
            trace_events: event_count,
            unique_facts: fact_tree.page_count(),
            fact_pages: fact_tree.page_count(),
            fact_encoded_bytes: fact_tree.encoded_bytes(),
            workspace_pages_scanned,
        };
        Ok(VerifiedUnitReadClosure {
            package_target: self.package_target,
            invocation_recipe: self.invocation_recipe,
            capture_identity: self.capture_identity,
            adapter_protocol: self.protocol.identity,
            fact_tree,
            closure_root,
            stats,
        })
    }

    fn check_healthy(&self) -> Result<(), CompilerUnitReadClosureErrorV2> {
        self.failure.map_or(Ok(()), |reason| {
            Err(CompilerUnitReadClosureErrorV2::Incomplete(reason))
        })
    }

    fn poison<T>(
        &mut self,
        reason: CompilerReadClosureIncompleteReasonV2,
    ) -> Result<T, CompilerUnitReadClosureErrorV2> {
        self.failure = Some(reason);
        Err(CompilerUnitReadClosureErrorV2::Incomplete(reason))
    }
}

fn derive_closure_root(
    protocol: CompilerReadAdapterProtocolIdentityV2,
    fact_root: [u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(UNIT_READ_CLOSURE_DOMAIN);
    hasher.update(protocol.as_bytes());
    hasher.update(&fact_root);
    *hasher.finalize().as_bytes()
}

fn target_matches_profile(target: &CompilerPackageTargetV2, profile: LanguageProfile) -> bool {
    match target.unit_key() {
        crate::compiler_input_manifest_v2::CompilationUnitKeyV2::PackageRoot => true,
        crate::compiler_input_manifest_v2::CompilationUnitKeyV2::RustCrate { .. } => {
            profile.language() == Language::Rust
        }
        crate::compiler_input_manifest_v2::CompilationUnitKeyV2::TypeScriptProgram { .. } => {
            profile.language() == Language::TypeScript
        }
        crate::compiler_input_manifest_v2::CompilationUnitKeyV2::PythonModule { .. } => {
            profile.language() == Language::Python
        }
        crate::compiler_input_manifest_v2::CompilationUnitKeyV2::GoPackage { .. } => {
            profile.language() == Language::Go
        }
        crate::compiler_input_manifest_v2::CompilationUnitKeyV2::JavaModule { .. } => {
            profile.language() == Language::Java
        }
        crate::compiler_input_manifest_v2::CompilationUnitKeyV2::CSharpProject { .. } => {
            profile.language() == Language::CSharp
        }
        crate::compiler_input_manifest_v2::CompilationUnitKeyV2::ClangTranslationUnit {
            ..
        } => profile.language() == Language::Clang,
    }
}

fn derive_pure_unit_key(
    target: backend_version::ContentId<CompilationTargetDomain>,
    recipe: CompilerInvocationRecipeV2,
    closure_root: [u8; 32],
) -> PureUnitKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PURE_UNIT_KEY_DOMAIN);
    hasher.update(target.as_ref());
    hasher.update(recipe.identity().as_ref());
    hasher.update(&closure_root);
    PureUnitKey(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler_input_manifest_v2::{
        CompilationUnitKeyV2, CompilerInputManifestV2, CompilerInvocationRecipeV2,
        CompilerPackageTargetV2,
    };
    use crate::compiler_input_tree_v2::{
        CompilerInputTreeRecordV2 as Record, CompilerWorkspaceFileRoleV2 as FileRole,
    };
    use backend_semantic::vocabulary::PackageUrl;
    use backend_semantic::vocabulary::{
        LanguageProfile, NativeTool, RustEdition, TypeScriptSource,
    };
    use backend_version::{ContentId, ToolchainDomain};

    fn recipe() -> CompilerInvocationRecipeV2 {
        CompilerInvocationRecipeV2::new(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"rustc 1.88 / sysroot digest"),
            [2; 32],
            [3; 32],
            [4; 32],
        )
        .expect("portable recipe")
    }

    fn target() -> CompilerPackageTargetV2 {
        CompilerPackageTargetV2::new(
            PackageUrl::parse("pkg:cargo/read-closure-fixture@1.0.0".to_owned())
                .expect("package URL"),
            CompilationUnitKeyV2::RustCrate {
                name: "lib".into(),
                root: "src/lib.rs".into(),
            },
        )
        .expect("unit target")
    }

    fn workspace(with_late_file: bool) -> CompilerInputMerkleTreeV2 {
        let mut records = vec![
            Record::Directory { path: "".into() },
            Record::Directory { path: "src".into() },
            Record::File {
                path: "src/lib.rs".into(),
                role: FileRole::Source,
                object_id: [0x11; 32],
                length: 24,
            },
        ];
        if with_late_file {
            records.push(Record::File {
                path: "src/optional.rs".into(),
                role: FileRole::Source,
                object_id: [0x22; 32],
                length: 7,
            });
        }
        records.sort_by(compare_records);
        CompilerInputMerkleTreeV2::from_sorted_records(CompilerInputTreeKindV2::Workspace, records)
            .expect("workspace tree")
    }

    fn manifest_for_workspace(
        root: [u8; 32],
        source_provenance: [u8; 32],
    ) -> CompilerInputManifestV2 {
        CompilerInputManifestV2::new(
            target(),
            [0x31; 32],
            recipe(),
            source_provenance,
            "fixture-policy.v1",
            [0x41; 32],
            root,
            1024 * 1024,
        )
        .expect("workspace capture manifest")
    }

    fn reviewed_protocol() -> TrustedCompilerReadAdapterProtocolV2 {
        TrustedCompilerReadAdapterProtocolV2::fixture("fixture-trace", 1, [0x51; 32])
            .expect("reviewed test adapter protocol")
    }

    fn reviewed_protocol_with_digest(digest: [u8; 32]) -> TrustedCompilerReadAdapterProtocolV2 {
        TrustedCompilerReadAdapterProtocolV2::fixture("fixture-trace", 1, digest)
            .expect("reviewed test adapter protocol")
    }

    fn trace(
        manifest: &CompilerInputManifestV2,
        workspace: &CompilerInputMerkleTreeV2,
    ) -> Result<VerifiedUnitReadClosure, CompilerUnitReadClosureErrorV2> {
        trace_with_protocol(reviewed_protocol(), manifest, workspace)
    }

    fn trace_with_protocol(
        protocol: TrustedCompilerReadAdapterProtocolV2,
        manifest: &CompilerInputManifestV2,
        workspace: &CompilerInputMerkleTreeV2,
    ) -> Result<VerifiedUnitReadClosure, CompilerUnitReadClosureErrorV2> {
        let mut builder = CompilerReadTraceBuilderV2::for_manifest(protocol, manifest);
        builder.begin()?;
        let listing_digest = workspace.directory_listing_digest("src")?;
        let facts = [
            Record::PresentFile {
                path: "src/lib.rs".into(),
                object_id: [0x11; 32],
                length: 24,
            },
            Record::AbsentPath {
                path: "src/generated.rs".into(),
            },
            Record::DirectoryListing {
                path: "src".into(),
                listing_digest,
            },
            Record::EnvironmentVariablePresent {
                name: "RUSTFLAGS".into(),
                value_digest: [0x61; 32],
            },
            Record::EnvironmentVariableAbsent {
                name: "CARGO_ENCODED_RUSTFLAGS".into(),
            },
            Record::GeneratedInput {
                key: "out/build_config.rs".into(),
                content_digest: [0x71; 32],
                length: 15,
            },
            Record::ToolchainInput {
                key: "bin/rustc".into(),
                content_digest: [0x81; 32],
                length: 32,
            },
            Record::ExternalInput {
                key: "registry/crate.rmeta".into(),
                content_digest: [0x91; 32],
                length: 48,
            },
        ];
        for (sequence, fact) in facts.into_iter().enumerate() {
            builder.observe(sequence as u64, fact)?;
        }
        builder.end(8)?;
        builder.finish(workspace)
    }

    fn trace_with_facts(
        manifest: &CompilerInputManifestV2,
        workspace: &CompilerInputMerkleTreeV2,
        facts: Vec<Record>,
        independently_reported_event_count: u64,
    ) -> Result<VerifiedUnitReadClosure, CompilerUnitReadClosureErrorV2> {
        let mut builder = CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), manifest);
        builder.begin()?;
        for (sequence, fact) in facts.into_iter().enumerate() {
            builder.observe(sequence as u64, fact)?;
        }
        builder.end(independently_reported_event_count)?;
        builder.finish(workspace)
    }

    fn independent_oracle_contains_all(
        closure: &VerifiedUnitReadClosure,
        expected: &[Record],
    ) -> bool {
        // Deliberately use a linear equality scan over an independently-authored fixture list;
        // this does not share the production tree's path ordering or deduplication logic.
        expected.iter().all(|expected_fact| {
            closure
                .fact_tree()
                .records()
                .any(|observed_fact| observed_fact == expected_fact)
        })
    }

    #[test]
    fn observation_recorder_seals_only_registered_channel_events_and_stays_partial() {
        let mut recorder = CompilerReadObservationRecorderV2::new().expect("attempt id");
        let producer = recorder
            .register(CompilerReadObservationChannelV2::EditorOverlay)
            .expect("editor buffer producer");
        recorder
            .observe_editor_buffer(&producer, 0, "src/lib.rs", b"pub fn value() {}\n")
            .expect("first selected buffer");
        recorder
            .observe_editor_buffer(&producer, 1, "src/main.rs", b"fn main() {}\n")
            .expect("second selected buffer");
        recorder.seal(&producer, 2).expect("producer seal");

        let report = recorder.report();
        assert_eq!(report.events(), 2);
        assert_eq!(report.registered_channels(), 1);
        assert_eq!(report.sealed_channels(), 1);
        assert_eq!(report.required_channels(), 11);
        assert!(!report.all_required_producers_sealed());
        assert!(
            report
                .missing_channel_labels()
                .contains(&"rust_module_resolver")
        );
        assert!(report.missing_channel_labels().contains(&"process_tree"));
    }

    #[test]
    fn stale_read_observation_producer_is_rejected_by_attempt_fence() {
        let mut first = CompilerReadObservationRecorderV2::new().expect("first attempt");
        let producer = first
            .register(CompilerReadObservationChannelV2::EditorOverlay)
            .expect("producer");
        let second = CompilerReadObservationRecorderV2::new().expect("second attempt");
        let mut second = second;
        assert_eq!(
            second.observe_editor_buffer(&producer, 0, "src/lib.rs", b"fn main() {}"),
            Err(CompilerReadObservationFailureV2::StaleAttemptOrProducer)
        );
        assert_eq!(
            second.report().failure(),
            Some(CompilerReadObservationFailureV2::StaleAttemptOrProducer)
        );
    }

    #[test]
    fn unsupported_path_poisoning_prevents_a_channel_seal() {
        let mut recorder = CompilerReadObservationRecorderV2::new().expect("attempt id");
        let producer = recorder
            .register(CompilerReadObservationChannelV2::RustModuleResolver)
            .expect("module resolver producer");
        assert_eq!(
            recorder.observe_path_event(
                &producer,
                0,
                CompilerReadObservationEventClassV2::AbsentPath,
                "src/../outside.rs",
                *blake3::hash(b"absent").as_bytes(),
                0,
            ),
            Err(CompilerReadObservationFailureV2::UnsupportedPath)
        );
        assert_eq!(
            recorder.seal(&producer, 0),
            Err(CompilerReadObservationFailureV2::UnsupportedPath)
        );
    }

    #[test]
    fn independent_oracle_detects_an_omitted_negative_module_lookup() {
        let mut recorder = CompilerReadObservationRecorderV2::new().expect("attempt id");
        let producer = recorder
            .register(CompilerReadObservationChannelV2::RustModuleResolver)
            .expect("module resolver producer");
        let positive = *blake3::hash(b"contents of src/lib.rs").as_bytes();
        recorder
            .observe_path_event(
                &producer,
                0,
                CompilerReadObservationEventClassV2::ModuleResolution,
                "src/lib.rs",
                positive,
                0,
            )
            .expect("observed positive module resolution");
        recorder
            .seal(&producer, 1)
            .expect("producer terminal count");

        // Independent fixture oracle: `mod optional;` caused both the present
        // parent source and this absent candidate to be consulted. The fake
        // adapter intentionally omits the negative candidate but seals its own
        // internally consistent count, which cannot detect the omission alone.
        let independently_expected = [
            (
                CompilerReadObservationEventClassV2::ModuleResolution,
                "src/lib.rs",
            ),
            (
                CompilerReadObservationEventClassV2::AbsentPath,
                "src/optional.rs",
            ),
        ];
        assert_eq!(independently_expected.len(), 2);
        assert_eq!(
            recorder.channel_event_count(CompilerReadObservationChannelV2::RustModuleResolver),
            1
        );
        assert_ne!(
            recorder.channel_event_count(CompilerReadObservationChannelV2::RustModuleResolver),
            u64::try_from(independently_expected.len()).expect("small fixture count")
        );
    }

    #[test]
    fn child_event_loss_disagrees_with_independent_child_terminal_count() {
        let mut recorder = CompilerReadObservationRecorderV2::new().expect("attempt id");
        let producer = recorder
            .register(CompilerReadObservationChannelV2::ProcessTree)
            .expect("process-tree producer");
        let child_start = *blake3::hash(b"cargo child start").as_bytes();
        recorder
            .observe_nonpath_event(
                &producer,
                0,
                CompilerReadObservationEventClassV2::ChildProcess,
                child_start,
                0,
            )
            .expect("child start event");
        // The process supervisor independently reports both start and exit.
        // Losing the exit event must poison the broker at the seal boundary.
        assert_eq!(
            recorder.seal(&producer, 2),
            Err(CompilerReadObservationFailureV2::FinalCountMismatch)
        );
        assert_eq!(
            recorder.report().failure(),
            Some(CompilerReadObservationFailureV2::FinalCountMismatch)
        );
    }

    #[test]
    fn complete_closure_binds_target_recipe_capture_and_all_typed_facts() {
        let workspace = workspace(false);
        let manifest = manifest_for_workspace(workspace.root(), [0xa1; 32]);
        let closure = trace(&manifest, &workspace).expect("complete closure");

        assert_eq!(closure.package_target(), manifest.package_target());
        assert_eq!(closure.profile(), recipe().profile());
        assert_eq!(closure.stage(), recipe().stage());
        assert_eq!(
            closure.capture_identity().workspace_root(),
            workspace.root()
        );
        assert_eq!(
            closure.capture_identity().workspace_snapshot_id(),
            manifest.workspace_snapshot_id()
        );
        assert_eq!(
            manifest.read_frontier_status(),
            crate::compiler_input_manifest_v2::CompilerReadFrontierStatusV2::Unproven
        );
        assert_eq!(closure.fact_tree().page_count(), 8);
        assert_eq!(closure.stats().trace_events, 8);
        assert_eq!(closure.stats().unique_facts, 8);
        assert_eq!(
            closure.stats().workspace_pages_scanned,
            workspace.page_count()
        );
        assert_ne!(closure.closure_root(), closure.fact_tree().root());
        let reopened = CompilerInputMerkleTreeV2::reopen(
            CompilerInputTreeKindV2::ReadFrontier,
            closure.fact_tree().root(),
            closure.fact_tree().page_payloads(),
        )
        .expect("fact tree roundtrip");
        assert_eq!(&reopened, closure.fact_tree());

        let mut closure_oracle = blake3::Hasher::new();
        closure_oracle.update(UNIT_READ_CLOSURE_DOMAIN);
        closure_oracle.update(closure.adapter_protocol().as_bytes());
        closure_oracle.update(&closure.fact_tree().root());
        assert_eq!(
            closure.closure_root(),
            *closure_oracle.finalize().as_bytes()
        );
    }

    #[test]
    fn pure_unit_key_matches_independent_oracle_and_omits_capture_provenance() {
        let workspace = workspace(false);
        let first_manifest = manifest_for_workspace(workspace.root(), [0xa1; 32]);
        let second_manifest = manifest_for_workspace(workspace.root(), [0xa2; 32]);
        let first = trace(&first_manifest, &workspace).expect("first closure");
        let second = trace(&second_manifest, &workspace).expect("second closure");

        assert_ne!(
            first.capture_identity().workspace_snapshot_id(),
            second.capture_identity().workspace_snapshot_id()
        );
        assert_eq!(first.closure_root(), second.closure_root());
        assert_eq!(first.pure_unit_key(), second.pure_unit_key());

        let other_protocol = trace_with_protocol(
            reviewed_protocol_with_digest([0x52; 32]),
            &first_manifest,
            &workspace,
        )
        .expect("closure under another reviewed protocol");
        assert_ne!(first.closure_root(), other_protocol.closure_root());
        assert_ne!(first.pure_unit_key(), other_protocol.pure_unit_key());

        let mut oracle = blake3::Hasher::new();
        oracle.update(PURE_UNIT_KEY_DOMAIN);
        oracle.update(first_manifest.package_target().target().as_ref());
        oracle.update(first_manifest.invocation_recipe().identity().as_ref());
        oracle.update(&first.closure_root());
        assert_eq!(
            first.pure_unit_key().as_bytes(),
            oracle.finalize().as_bytes()
        );
    }

    #[test]
    fn negative_appearance_and_changed_listing_fail_closed() {
        let original = workspace(false);
        let changed = workspace(true);
        let changed_manifest = manifest_for_workspace(changed.root(), [0xa1; 32]);
        let mut negative =
            CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &changed_manifest);
        negative.begin().expect("start");
        negative
            .observe(
                0,
                Record::AbsentPath {
                    path: "src/optional.rs".into(),
                },
            )
            .expect("negative read");
        negative.end(1).expect("end");
        assert_eq!(
            negative.finish(&changed),
            Err(CompilerUnitReadClosureErrorV2::WorkspaceReadMismatch)
        );

        let changed_manifest = manifest_for_workspace(changed.root(), [0xa1; 32]);
        let mut listing =
            CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &changed_manifest);
        listing.begin().expect("start");
        listing
            .observe(
                0,
                Record::DirectoryListing {
                    path: "src".into(),
                    listing_digest: original
                        .directory_listing_digest("src")
                        .expect("old listing"),
                },
            )
            .expect("listing read");
        listing.end(1).expect("end");
        assert_eq!(
            listing.finish(&changed),
            Err(CompilerUnitReadClosureErrorV2::WorkspaceReadMismatch)
        );
    }

    #[test]
    fn independent_oracle_catches_omitted_negative_probe_and_lost_child_event() {
        let workspace = workspace(false);
        let manifest = manifest_for_workspace(workspace.root(), [0xa1; 32]);
        let expected = [
            Record::PresentFile {
                path: "src/lib.rs".into(),
                object_id: [0x11; 32],
                length: 24,
            },
            // The fixture's module-resolution oracle requires the failed candidate as well as
            // the source file. Workspace capture alone cannot infer this negative dependency.
            Record::AbsentPath {
                path: "src/optional.rs".into(),
            },
            // This record is emitted by the fixture's compiler child, not the parent authority.
            Record::ToolchainInput {
                key: "bin/rustc".into(),
                content_digest: [0x81; 32],
                length: 32,
            },
        ];

        let omitted_negative = trace_with_facts(
            &manifest,
            &workspace,
            vec![expected[0].clone(), expected[2].clone()],
            2,
        )
        .expect("the event stream is internally well-formed");
        assert!(!independent_oracle_contains_all(
            &omitted_negative,
            &expected
        ));

        // The child's sealed count comes from a separate producer fence. If its final event is
        // dropped before merge, the parent's locally observed sequence is shorter than that
        // count, so the closure builder must refuse to finish.
        assert_eq!(
            trace_with_facts(
                &manifest,
                &workspace,
                vec![expected[0].clone(), expected[1].clone()],
                3,
            ),
            Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::TruncatedOrDiscontinuousTrace
            ))
        );
    }

    #[test]
    fn missing_start_truncation_sequence_gaps_and_wrong_root_are_typed_incomplete() {
        let workspace = workspace(false);
        let manifest = manifest_for_workspace(workspace.root(), [0xa1; 32]);
        let missing = CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &manifest);
        assert_eq!(
            missing.finish(&workspace),
            Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::MissingInstrumentation
            ))
        );

        let mut unclosed = CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &manifest);
        unclosed.begin().expect("start");
        assert_eq!(
            unclosed.finish(&workspace),
            Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::MissingEndMarker
            ))
        );

        let mut silent = CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &manifest);
        silent.begin().expect("start");
        silent.end(0).expect("adapter reports no events");
        assert_eq!(
            silent.finish(&workspace),
            Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::MissingInstrumentation
            ))
        );

        let mut truncated =
            CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &manifest);
        truncated.begin().expect("start");
        truncated.mark_truncated();
        assert_eq!(
            truncated.finish(&workspace),
            Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::TruncatedOrDiscontinuousTrace
            ))
        );

        let mut gap = CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &manifest);
        gap.begin().expect("start");
        assert!(matches!(
            gap.observe(
                1,
                Record::AbsentPath {
                    path: "src/missing.rs".into(),
                }
            ),
            Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::TruncatedOrDiscontinuousTrace
            ))
        ));

        let wrong_capture = manifest_for_workspace([0xb9; 32], [0xa1; 32]);
        let mut wrong_root =
            CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &wrong_capture);
        wrong_root.begin().expect("start");
        wrong_root.end(0).expect("empty complete test trace");
        assert_eq!(
            wrong_root.finish(&workspace),
            Err(CompilerUnitReadClosureErrorV2::WorkspaceIdentityMismatch)
        );
    }

    #[test]
    fn unregistered_adapter_cannot_mint_a_complete_closure_capability() {
        assert_eq!(
            TrustedCompilerReadAdapterProtocolV2::registered("unreviewed-adapter", 1, [0x55; 32]),
            Err(CompilerUnitReadClosureErrorV2::Incomplete(
                CompilerReadClosureIncompleteReasonV2::UnregisteredAdapterProtocol
            ))
        );
    }

    #[test]
    fn repeated_identical_reads_collapse_but_conflicting_reads_reject() {
        let workspace = workspace(false);
        let manifest = manifest_for_workspace(workspace.root(), [0xa1; 32]);
        let mut repeated = CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &manifest);
        repeated.begin().expect("start");
        let fact = Record::PresentFile {
            path: "src/lib.rs".into(),
            object_id: [0x11; 32],
            length: 24,
        };
        repeated.observe(0, fact.clone()).expect("first read");
        repeated.observe(1, fact).expect("second read");
        repeated.end(2).expect("end");
        let closure = repeated.finish(&workspace).expect("deduplicated closure");
        assert_eq!(closure.stats().trace_events, 2);
        assert_eq!(closure.stats().unique_facts, 1);

        let mut conflicting =
            CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &manifest);
        conflicting.begin().expect("start");
        conflicting
            .observe(
                0,
                Record::PresentFile {
                    path: "src/lib.rs".into(),
                    object_id: [0x11; 32],
                    length: 24,
                },
            )
            .expect("first read");
        conflicting
            .observe(
                1,
                Record::PresentFile {
                    path: "src/lib.rs".into(),
                    object_id: [0x12; 32],
                    length: 24,
                },
            )
            .expect("second read");
        conflicting.end(2).expect("end");
        assert_eq!(
            conflicting.finish(&workspace),
            Err(CompilerUnitReadClosureErrorV2::WorkspaceReadMismatch)
        );
    }

    #[test]
    fn protocol_and_fact_namespaces_are_stable_and_case_safe() {
        let workspace = workspace(false);
        let manifest = manifest_for_workspace(workspace.root(), [0xa1; 32]);
        let mut facts = vec![
            Record::EnvironmentVariablePresent {
                name: "PATH".into(),
                value_digest: [0x61; 32],
            },
            Record::GeneratedInput {
                key: "out/generated.rs".into(),
                content_digest: [0x71; 32],
                length: 15,
            },
            Record::ToolchainInput {
                key: "bin/rustc".into(),
                content_digest: [0x81; 32],
                length: 32,
            },
            Record::ExternalInput {
                key: "registry/crate.rmeta".into(),
                content_digest: [0x91; 32],
                length: 48,
            },
        ];
        facts.sort_by(compare_records);
        assert!(
            CompilerInputMerkleTreeV2::from_sorted_records(
                CompilerInputTreeKindV2::ReadFrontier,
                facts.clone()
            )
            .is_ok()
        );
        facts.push(Record::EnvironmentVariableAbsent {
            name: "path".into(),
        });
        facts.sort_by(compare_records);
        assert_eq!(
            CompilerInputMerkleTreeV2::from_sorted_records(
                CompilerInputTreeKindV2::ReadFrontier,
                facts
            ),
            Err(CompilerInputTreeV2Error::NonCanonical)
        );
        let aliases = vec![
            Record::GeneratedInput {
                key: "out/Foo.rs".into(),
                content_digest: [0x71; 32],
                length: 15,
            },
            Record::GeneratedInput {
                key: "out/foo.rs".into(),
                content_digest: [0x72; 32],
                length: 15,
            },
        ];
        assert_eq!(
            CompilerInputMerkleTreeV2::from_sorted_records(
                CompilerInputTreeKindV2::ReadFrontier,
                aliases
            ),
            Err(CompilerInputTreeV2Error::NonCanonical)
        );
        assert_eq!(
            CompilerInputMerkleTreeV2::from_sorted_records(
                CompilerInputTreeKindV2::ReadFrontier,
                vec![Record::ExternalInput {
                    key: "../outside/file".into(),
                    content_digest: [0x91; 32],
                    length: 48,
                }]
            ),
            Err(CompilerInputTreeV2Error::NonCanonical)
        );
        assert!(
            CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &manifest)
                .protocol
                .identity
                .to_bytes()
                != [0; 32]
        );
    }

    #[test]
    fn native_unit_and_recipe_profiles_must_match_before_closure_admission() {
        let workspace = workspace(false);
        let typescript_recipe = CompilerInvocationRecipeV2::new(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::Parse,
            NativeTool::TypeScriptCompiler,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"tsc toolchain digest"),
            [0x12; 32],
            [0x13; 32],
            [0x14; 32],
        )
        .expect("TypeScript recipe");
        let wrong_profile_manifest = CompilerInputManifestV2::new(
            target(),
            [0x31; 32],
            typescript_recipe,
            [0xa1; 32],
            "fixture-policy.v1",
            [0x41; 32],
            workspace.root(),
            1024 * 1024,
        )
        .expect("manifest with mismatched target/profile");
        let mut builder =
            CompilerReadTraceBuilderV2::for_manifest(reviewed_protocol(), &wrong_profile_manifest);
        builder.begin().expect("start");
        builder.end(0).expect("end");
        assert_eq!(
            builder.finish(&workspace),
            Err(CompilerUnitReadClosureErrorV2::TargetProfileMismatch)
        );
    }
}
