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
use thiserror::Error;

const ADAPTER_PROTOCOL_DOMAIN: &[u8] = b"backend.compiler.read-adapter-protocol.v2\0";
const UNIT_READ_CLOSURE_DOMAIN: &[u8] = b"backend.compiler.verified-unit-read-closure.v2\0";
const PURE_UNIT_KEY_DOMAIN: &[u8] = b"backend.compiler.pure-unit-key.v2\0";
const MAX_ADAPTER_NAME_BYTES: usize = 128;
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
