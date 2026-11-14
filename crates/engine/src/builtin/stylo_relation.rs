//! Canonical, version-fenced native Stylo analysis rows.
//!
//! A row is a producer claim. Authentication of captured bytes, exact
//! predecessor/target updates, closure membership and cross-row ownership is
//! required before serving this relation.
use super::{StyloProjectedRow, StyloWorkerPolicy, stylo_projection::bounded_encode};
use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_semantic::stylo_analysis::{
    StyloCoverage, StyloParseConfig, StyloPreferenceSnapshot, StyloPreferenceStability,
    StyloSourceGap,
};
use backend_store::TypedObject;
use backend_version::{
    CanonicalRelation, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity, StateRoot,
};
use serde::{Deserialize, Serialize};

const NATIVE_MAGIC: &[u8; 4] = b"STN2";
const ROOT_KEY: [u8; 32] = [0x41; 32];
/// Maximum encoded canonical native Stylo row bytes.
pub const MAX_STYLO_RELATION_ROW_BYTES: usize = 32 * 1024;

/// Native Stylo analysis relation; IDs are provisional pending registry integration.
#[derive(Debug)]
pub struct StyloAnalysisRelation;
/// Typed pointer to the selected native Stylo analysis root.
pub struct StyloAnalysisRootSchema;
impl Schema for StyloAnalysisRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 13;
    const VERSION: u8 = 2;
    type Value = [u8; 32];
    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Native producer profile selected by this relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloAnalysisProfile {
    /// Standalone author stylesheet parsed by the declared native Stylo recipe.
    StyloStandaloneV1,
}

/// Discovery completeness is independent of parse and semantic capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloInventory {
    Complete,
    Incomplete,
}
/// One captured host's native analysis terminal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloTerminal {
    /// Native stylesheet projection, with its own partial/complete state.
    Parsed(StyloCoverage),
    /// The native attempt could not produce facts.
    Unavailable(StyloUnavailable),
}
/// A failed native host attempt never means known-empty.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloUnavailable {
    /// Selected host bytes could not be read.
    ReadUnavailable,
    /// A complete enough project selection or policy witness was unavailable.
    SelectionUnavailable,
    /// Captured bytes are not valid standalone UTF-8.
    InvalidUtf8,
    /// Input, output, row, or other bounded work was exhausted.
    Limit,
    /// A native worker is not admitted or available.
    ParserUnavailable,
    /// The requested hard process resource guarantee is unsupported here.
    UnsupportedHardLimit,
    /// The supervised native worker exceeded its wall-time budget.
    Deadline,
    /// The native process exited or returned an invalid bounded protocol frame.
    WorkerFailure,
    /// Process-global preference values changed across the parse.
    PreferencesChanged,
}

/// Whether a trusted native worker capability was available to the attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloAttemptArtifact {
    /// Exact admitted executable and support closure identity.
    Admitted([u8; 32]),
    /// No admitted capability was supplied; this says nothing about disk paths.
    NoPermit,
    /// Artifact admission state was not witnessed.
    Unknown,
}

/// Status of the most recent same-walk native analysis attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloAttemptOutcome {
    /// A selected native inventory and host terminals were captured.
    Observed,
    /// No new native selection was captured; linked facts are last-served.
    Unavailable(StyloUnavailable),
}

/// Project selection, captured host or native semantic claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum StyloAnalysisRecord {
    /// Selects a bounded observed inventory and records actual parser inputs.
    Selection {
        project: [u8; 32],
        source_root: String,
        profile: StyloAnalysisProfile,
        recipe: String,
        producer: [u8; 32],
        worker_artifact: [u8; 32],
        adapter: [u8; 32],
        discovery: [u8; 32],
        policy_complete: bool,
        /// Fixed worker-attempt cap; absent on historical v2 rows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_worker_attempts: Option<u32>,
        inventory: StyloInventory,
        inventory_digest: [u8; 32],
        inventory_count: u64,
        host_count: u32,
        host_digest: [u8; 32],
    },
    /// Latest same-walk attempt; absence in historical v2 roots is Unknown.
    Attempt {
        project: [u8; 32],
        source_root: String,
        profile: StyloAnalysisProfile,
        /// Canonical requested discovery and resource policy inputs.
        scope_policy: [u8; 32],
        /// Explicitly unknown when policy files could not be witnessed.
        discovery: Option<[u8; 32]>,
        policy_complete: bool,
        /// Fixed worker-attempt cap; absent on historical v2 attempts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_worker_attempts: Option<u32>,
        inventory: Option<StyloInventory>,
        inventory_digest: Option<[u8; 32]>,
        inventory_count: Option<u64>,
        expected_producer: Option<[u8; 32]>,
        expected_recipe: Option<String>,
        adapter: [u8; 32],
        worker_artifact: StyloAttemptArtifact,
        /// Requested bounds, not a claim that a process ran or a limit was enforced.
        requested_policy: StyloWorkerPolicy,
        outcome: StyloAttemptOutcome,
        /// Digest of the canonical retained Selection row, not a global root.
        served_selection: Option<[u8; 32]>,
    },
    /// Authenticated capture identity or a qualified unavailable terminal.
    Host {
        project: [u8; 32],
        path: String,
        content: Option<[u8; 32]>,
        source: Option<[u8; 32]>,
        byte_length: Option<u64>,
        utf8_bom_removed: Option<bool>,
        config: Option<StyloParseConfig>,
        configuration: Option<[u8; 32]>,
        preferences: Option<StyloPreferenceSnapshot>,
        preference_stability: Option<StyloPreferenceStability>,
        analysis_basis: Option<[u8; 32]>,
        worker_policy: Option<StyloWorkerPolicy>,
        source_coverage: Option<StyloCoverage>,
        source_gaps: Vec<StyloSourceGap>,
        source_observer_recipe: Option<String>,
        terminal: StyloTerminal,
        fact_count: u32,
        fact_digest: [u8; 32],
    },
    /// One native rule, selector, retained declaration, dependency or diagnostic.
    Fact {
        host: [u8; 32],
        ordinal: u32,
        subordinal: u32,
        fact: StyloProjectedRow,
    },
}
impl StyloAnalysisRecord {
    /// Checks local shape and encoded size, not cross-row or host authentication.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_shape()?;
        bounded_encode(self, MAX_STYLO_RELATION_ROW_BYTES - NATIVE_MAGIC.len())?;
        Ok(())
    }
    fn validate_shape(&self) -> Result<(), String> {
        match self {
            Self::Selection {
                project,
                source_root,
                recipe,
                producer,
                worker_artifact,
                adapter,
                max_worker_attempts,
                ..
            } if *project == [0; 32]
                || *producer == [0; 32]
                || *worker_artifact == [0; 32]
                || *adapter == [0; 32]
                || max_worker_attempts.is_some_and(|value| value == 0 || value > 1024)
                || source_root.is_empty()
                || source_root.len() > 4096
                || source_root.contains('\0')
                || recipe.is_empty()
                || recipe.len() > 4096 =>
            {
                return Err("invalid Stylo selection".into());
            }
            Self::Attempt {
                project,
                source_root,
                scope_policy,
                discovery,
                policy_complete,
                max_worker_attempts,
                inventory,
                inventory_digest,
                inventory_count,
                expected_producer,
                expected_recipe,
                adapter,
                worker_artifact,
                requested_policy,
                outcome,
                served_selection,
                ..
            } if *project == [0; 32]
                || *scope_policy == [0; 32]
                || max_worker_attempts.is_some_and(|value| value == 0 || value > 1024)
                || *adapter == [0; 32]
                || expected_recipe
                    .as_ref()
                    .is_some_and(|recipe| recipe.is_empty() || recipe.len() > 4096)
                || source_root.is_empty()
                || source_root.len() > 4096
                || source_root.contains('\0')
                || discovery.is_some_and(|value| value == [0; 32])
                || (*policy_complete && discovery.is_none())
                || expected_producer.is_some() != expected_recipe.is_some()
                || inventory.is_some() != inventory_digest.is_some()
                || inventory.is_some() != inventory_count.is_some()
                || inventory_digest.is_some_and(|value| value == [0; 32])
                || expected_producer.is_some_and(|value| value == [0; 32])
                || matches!(worker_artifact, StyloAttemptArtifact::Admitted(value) if *value == [0; 32])
                || (matches!(worker_artifact, StyloAttemptArtifact::NoPermit)
                    && *outcome
                        != StyloAttemptOutcome::Unavailable(
                            StyloUnavailable::ParserUnavailable,
                        ))
                || (matches!(worker_artifact, StyloAttemptArtifact::Admitted(_))
                    && expected_producer.is_none())
                || requested_policy.validate().is_err()
                || served_selection.is_some_and(|value| value == [0; 32])
                || (matches!(outcome, StyloAttemptOutcome::Observed)
                    && (discovery.is_none()
                        || inventory.is_none()
                        || expected_producer.is_none()
                        || expected_recipe.is_none()
                        || !matches!(worker_artifact, StyloAttemptArtifact::Admitted(_))
                        || served_selection.is_none())) =>
            {
                return Err("invalid Stylo latest attempt".into());
            }
            Self::Host {
                project,
                path,
                content,
                source,
                byte_length,
                utf8_bom_removed,
                config,
                configuration,
                preferences,
                preference_stability,
                analysis_basis,
                worker_policy,
                source_coverage,
                source_gaps,
                source_observer_recipe,
                terminal,
                fact_count,
                ..
            } if *project == [0; 32]
                || !canonical_path(path)
                || content.is_some() != source.is_some()
                || content.is_some() != byte_length.is_some()
                || config.is_some() != configuration.is_some()
                || config.is_some() != worker_policy.is_some()
                || configuration.is_some_and(|value| value == [0; 32])
                || preferences.is_some() != preference_stability.is_some()
                || content.is_some_and(|value| value == [0; 32])
                || source.is_some_and(|value| value == [0; 32])
                || analysis_basis.is_some_and(|value| value == [0; 32])
                || (matches!(terminal, StyloTerminal::Parsed(_))
                    && (content.is_none()
                        || utf8_bom_removed.is_none()
                        || config.is_none()
                        || configuration.is_none()
                        || preferences.is_none()
                        || preference_stability.is_none()
                        || analysis_basis.is_none()
                        || worker_policy.is_none()
                        || *source_coverage != Some(StyloCoverage::Partial)
                        || source_observer_recipe
                            .as_ref()
                            .is_none_or(|recipe| recipe.is_empty() || recipe.len() > 4096)
                        || source_gaps.len() > 4))
                || (matches!(terminal, StyloTerminal::Parsed(StyloCoverage::Complete))
                    && *preference_stability == Some(StyloPreferenceStability::Unknown))
                || (matches!(terminal, StyloTerminal::Unavailable(_)) && *fact_count != 0) =>
            {
                return Err("invalid Stylo host".into());
            }
            Self::Fact { host, .. } if *host == [0; 32] => {
                return Err("invalid Stylo fact owner".into());
            }
            _ => {}
        }
        if let Self::Host {
            worker_policy: Some(policy),
            ..
        } = self
        {
            policy.validate()?;
        }
        Ok(())
    }
}
fn canonical_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !(path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic())
        && !path.contains('\\')
        && !path.bytes().any(|byte| byte.is_ascii_control())
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
impl Relation for StyloAnalysisRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 12;
    const VERSION: u8 = 2;
    type Key = [u8; 32];
    type Value = StyloAnalysisRecord;
    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(NATIVE_MAGIC);
        output.extend_from_slice(&serde_json::to_vec(value).expect("serializable Stylo record"));
    }
}
impl CanonicalRelation for StyloAnalysisRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes.try_into().map_err(|_| RelationDecodeError::Malformed)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        if bytes.len() > MAX_STYLO_RELATION_ROW_BYTES || !bytes.starts_with(NATIVE_MAGIC) {
            return Err(RelationDecodeError::Malformed);
        }
        let value: StyloAnalysisRecord = serde_json::from_slice(&bytes[NATIVE_MAGIC.len()..])
            .map_err(|_| RelationDecodeError::Malformed)?;
        value
            .validate()
            .map_err(|_| RelationDecodeError::Malformed)?;
        let mut canonical = Vec::new();
        Self::encode_value(&value, &mut canonical);
        if canonical != bytes {
            return Err(RelationDecodeError::Malformed);
        }
        Ok(value)
    }
}
/// Derives the selection row key for one project and native Stylo profile.
pub fn stylo_selection_key(project: [u8; 32]) -> [u8; 32] {
    stylo_owner_key(b"selection", &project, b"")
}
/// Derives the sole latest-attempt row key for a native CSS project.
/// The owner domain explicitly names StyloStandaloneV1; another profile must
/// use a distinct derivation and admitted schema.
pub fn stylo_attempt_key(project: [u8; 32]) -> [u8; 32] {
    stylo_owner_key(b"attempt", &project, b"")
}

/// Derives the host row key from the project and normalized relative path.
///
/// # Errors
/// Rejects a malformed or noncanonical project-relative path.
pub fn stylo_host_key(project: [u8; 32], path: &str) -> Result<[u8; 32], String> {
    if project == [0; 32] || !canonical_path(path) {
        return Err("invalid Stylo host key".into());
    }
    Ok(stylo_owner_key(b"host", &project, path.as_bytes()))
}
fn stylo_owner_key(kind: &[u8], project: &[u8; 32], suffix: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-standalone-v1.owner\0");
    hash.update(&(kind.len() as u64).to_be_bytes());
    hash.update(kind);
    hash.update(project);
    hash.update(&(suffix.len() as u64).to_be_bytes());
    hash.update(suffix);
    *hash.finalize().as_bytes()
}
/// Incremental witness of the complete actually enumerated CSS path set.
/// Paths must arrive in strictly increasing canonical order; an incomplete
/// source walk remains explicitly incomplete even when this witness is valid.
pub struct StyloInventoryWitness {
    hash: blake3::Hasher,
    previous: Option<String>,
    count: u64,
}
impl Default for StyloInventoryWitness {
    fn default() -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"nudox.stylo-standalone-v1.inventory\0");
        Self {
            hash,
            previous: None,
            count: 0,
        }
    }
}
impl StyloInventoryWitness {
    /// Adds one selected path without retaining the entire path inventory.
    ///
    /// # Errors
    /// Rejects invalid, duplicate, out-of-order, or excessive input.
    pub fn push(&mut self, path: &str) -> Result<(), String> {
        if !canonical_path(path) || self.previous.as_deref().is_some_and(|last| last >= path) {
            return Err("invalid Stylo inventory order or path".into());
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or("Stylo inventory count overflow")?;
        self.hash.update(&(path.len() as u64).to_be_bytes());
        self.hash.update(path.as_bytes());
        self.previous = Some(path.to_owned());
        Ok(())
    }
    /// Returns the number of paths witnessed and a deterministic digest.
    pub fn finish(mut self) -> (u64, [u8; 32]) {
        self.hash.update(&self.count.to_be_bytes());
        (self.count, *self.hash.finalize().as_bytes())
    }
}

/// Digest of bounded, strictly key-ordered canonical rows. The caller supplies
/// only rows selected for the witness; this does not prove relation membership.
///
/// # Errors
/// Rejects unordered keys, invalid rows, or more than 64 MiB encoded work.
pub fn stylo_rows_digest(rows: &[([u8; 32], &StyloAnalysisRecord)]) -> Result<[u8; 32], String> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-standalone-v1.rows\0");
    let mut prior = None;
    let mut bytes = 0usize;
    for (key, value) in rows {
        if prior.is_some_and(|previous| previous >= *key) {
            return Err("Stylo rows are not strictly ordered".into());
        }
        value.validate_shape()?;
        let encoded = bounded_encode(value, MAX_STYLO_RELATION_ROW_BYTES - NATIVE_MAGIC.len())?;
        bytes = bytes
            .checked_add(encoded.len() + NATIVE_MAGIC.len())
            .ok_or("Stylo row work overflow")?;
        if bytes > super::MAX_STYLO_PROJECTED_BYTES {
            return Err("Stylo rows exceed work budget".into());
        }
        hash.update(key);
        hash.update(&((encoded.len() + NATIVE_MAGIC.len()) as u64).to_be_bytes());
        hash.update(NATIVE_MAGIC);
        hash.update(&encoded);
        prior = Some(*key);
    }
    hash.update(&(rows.len() as u64).to_be_bytes());
    Ok(*hash.finalize().as_bytes())
}

/// Canonical key of the selected native Stylo relation pointer.
#[must_use]
pub fn stylo_analysis_root_pointer_key() -> [u8; 32] {
    *ObjectKey::<StyloAnalysisRootSchema>::from_value(&ROOT_KEY).as_bytes()
}

/// Constructs the typed closure pointer for an admitted native relation.
pub fn stylo_analysis_root_object(root: StateRoot<StyloAnalysisRelation>) -> TypedObject {
    let key = ObjectKey::<StyloAnalysisRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}
/// Opens the selected v2 pointer. Missing is unknown, not an empty stylesheet set.
pub fn stylo_analysis_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceRelationHandle<StyloAnalysisRelation>>, WorkspaceRelationError> {
    let key = ObjectKey::<StyloAnalysisRootSchema>::from_value(&ROOT_KEY);
    snapshot.auxiliary_relation::<StyloAnalysisRelation>(
        SchemaIdentity::new(
            StyloAnalysisRootSchema::DOMAIN,
            StyloAnalysisRootSchema::TYPE,
            StyloAnalysisRootSchema::VERSION,
        ),
        *key.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    struct QuarantinedV1;
    impl Schema for QuarantinedV1 {
        const DOMAIN: u8 = 0x97;
        const TYPE: u16 = 13;
        const VERSION: u8 = 1;
        type Value = [u8; 32];
        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }
    #[test]
    fn unavailable_latest_attempt_is_explicit_and_cannot_forge_a_selection_link() {
        let project = [1; 32];
        let mut attempt = StyloAnalysisRecord::Attempt {
            project,
            source_root: "/source".into(),
            profile: StyloAnalysisProfile::StyloStandaloneV1,
            scope_policy: [4; 32],
            discovery: None,
            policy_complete: false,
            max_worker_attempts: Some(16),
            inventory: None,
            inventory_digest: None,
            inventory_count: None,
            expected_producer: None,
            expected_recipe: None,
            adapter: [5; 32],
            worker_artifact: StyloAttemptArtifact::NoPermit,
            requested_policy: StyloWorkerPolicy {
                wall_time_nanos: 1,
                input_bytes: 1024,
                stdout_bytes: 1024,
                stderr_bytes: 64,
                output_bytes: 2048,
                memory_bytes: None,
            },
            outcome: StyloAttemptOutcome::Unavailable(StyloUnavailable::ParserUnavailable),
            served_selection: None,
        };
        let key = stylo_attempt_key(project);
        assert!(admit_stylo_relation(&[(key, &attempt)]).is_ok());
        let mut bytes = Vec::new();
        StyloAnalysisRelation::encode_value(&attempt, &mut bytes);
        assert_eq!(
            StyloAnalysisRelation::decode_value(&bytes).unwrap(),
            attempt
        );
        let mut historical = attempt.clone();
        if let StyloAnalysisRecord::Attempt {
            max_worker_attempts,
            ..
        } = &mut historical
        {
            *max_worker_attempts = None;
        }
        let mut historical_bytes = Vec::new();
        StyloAnalysisRelation::encode_value(&historical, &mut historical_bytes);
        assert!(
            !std::str::from_utf8(&historical_bytes)
                .unwrap()
                .contains("max_worker_attempts")
        );
        assert_eq!(
            StyloAnalysisRelation::decode_value(&historical_bytes).unwrap(),
            historical
        );
        let mut unknown = attempt.clone();
        if let StyloAnalysisRecord::Attempt {
            worker_artifact, ..
        } = &mut unknown
        {
            *worker_artifact = StyloAttemptArtifact::Unknown;
        }
        assert!(admit_stylo_relation(&[(key, &unknown)]).is_ok());
        let mut unknown_bytes = Vec::new();
        StyloAnalysisRelation::encode_value(&unknown, &mut unknown_bytes);
        assert_ne!(
            bytes, unknown_bytes,
            "NoPermit and Unknown are distinct states"
        );
        if let StyloAnalysisRecord::Attempt {
            served_selection, ..
        } = &mut attempt
        {
            *served_selection = Some([3; 32]);
        }
        assert!(admit_stylo_relation(&[(key, &attempt)]).is_err());
        if let StyloAnalysisRecord::Attempt {
            served_selection,
            outcome,
            ..
        } = &mut attempt
        {
            *served_selection = None;
            *outcome = StyloAttemptOutcome::Observed;
        }
        assert!(admit_stylo_relation(&[(key, &attempt)]).is_err());
    }

    #[test]
    fn worker_attempt_policy_changes_root_and_legacy_selection_roundtrips() {
        let project = [1; 32];
        let (_, inventory_digest) = StyloInventoryWitness::default().finish();
        let old = StyloAnalysisRecord::Selection {
            project,
            source_root: "/project".into(),
            profile: StyloAnalysisProfile::StyloStandaloneV1,
            recipe: "stylo-test".into(),
            producer: [2; 32],
            worker_artifact: [3; 32],
            adapter: [4; 32],
            discovery: [5; 32],
            policy_complete: false,
            max_worker_attempts: None,
            inventory: StyloInventory::Complete,
            inventory_digest,
            inventory_count: 0,
            host_count: 0,
            host_digest: stylo_rows_digest(&[]).unwrap(),
        };
        let mut old_bytes = Vec::new();
        StyloAnalysisRelation::encode_value(&old, &mut old_bytes);
        assert!(
            !std::str::from_utf8(&old_bytes)
                .unwrap()
                .contains("max_worker_attempts")
        );
        assert_eq!(
            StyloAnalysisRelation::decode_value(&old_bytes).unwrap(),
            old
        );
        let mut first = old.clone();
        let mut second = old;
        if let StyloAnalysisRecord::Selection {
            max_worker_attempts,
            ..
        } = &mut first
        {
            *max_worker_attempts = Some(16);
        }
        if let StyloAnalysisRecord::Selection {
            max_worker_attempts,
            ..
        } = &mut second
        {
            *max_worker_attempts = Some(15);
        }
        let coverage = super::super::complete_coverage(
            backend_execution::AuthorityVersion::from_value(b"Stylo cap test"),
        )
        .unwrap();
        let first_root = backend_version::RelationState::<StyloAnalysisRelation>::from_entries(
            [(stylo_selection_key(project), first)],
            coverage.clone(),
        )
        .unwrap();
        let second_root = backend_version::RelationState::<StyloAnalysisRelation>::from_entries(
            [(stylo_selection_key(project), second)],
            coverage,
        )
        .unwrap();
        assert_ne!(first_root.root(), second_root.root());
    }

    #[test]
    fn inventory_witness_changes_when_an_extra_selected_path_is_omitted_from_rows() {
        let mut earlier = StyloInventoryWitness::default();
        earlier.push("a.css").unwrap();
        let earlier = earlier.finish();
        let mut later = StyloInventoryWitness::default();
        later.push("a.css").unwrap();
        later.push("z.css").unwrap();
        let later = later.finish();
        assert_ne!(earlier, later);
        let mut invalid = StyloInventoryWitness::default();
        invalid.push("a.css").unwrap();
        assert!(invalid.push("a.css").is_err());
        assert!(invalid.push("../z.css").is_err());
    }
    #[test]
    fn row_witness_refuses_unordered_or_mutated_contents() {
        let row = StyloAnalysisRecord::Host {
            project: [1; 32],
            path: "site.css".into(),
            content: None,
            source: None,
            byte_length: None,
            utf8_bom_removed: None,
            config: None,
            configuration: None,
            preferences: None,
            preference_stability: None,
            analysis_basis: None,
            worker_policy: None,
            source_coverage: None,
            source_gaps: vec![],
            source_observer_recipe: None,
            terminal: StyloTerminal::Unavailable(StyloUnavailable::ReadUnavailable),
            fact_count: 0,
            fact_digest: [2; 32],
        };
        let mut changed = row.clone();
        if let StyloAnalysisRecord::Host { path, .. } = &mut changed {
            *path = "other.css".into();
        }
        assert_ne!(
            stylo_rows_digest(&[([1; 32], &row)]).unwrap(),
            stylo_rows_digest(&[([1; 32], &changed)]).unwrap()
        );
        assert!(stylo_rows_digest(&[([2; 32], &row), ([1; 32], &row)]).is_err());
    }
    #[test]
    fn native_selection_rejects_forged_fact_even_with_recomputed_witnesses() {
        use backend_semantic::stylo_analysis::{
            StyloAnalysis, StyloLimits, StyloPreferenceSnapshot, StyloPreferenceStability,
            StyloRule, StyloRuleKind,
        };
        let config = StyloParseConfig {
            source_kind: backend_semantic::stylo_analysis::StyloSourceKind::StandaloneUtf8,
            base_url: "https://example.invalid/a.css".into(),
            author_origin: true,
            no_quirks: true,
            allow_imports: false,
            limits: StyloLimits {
                input_bytes: 1024,
                rules: 10,
                depth: 10,
                records: 10,
                diagnostics: 10,
                text_bytes: 1024,
            },
        };
        let preferences = StyloPreferenceSnapshot {
            booleans: vec![true],
            integers: vec![1],
        };
        let recipe = "stylo-test".to_owned();
        let analysis = StyloAnalysis {
            config: config.clone(),
            recipe: recipe.clone(),
            support_fingerprint: [3; 32],
            utf8_bom_removed: false,
            preferences: preferences.clone(),
            preference_stability: StyloPreferenceStability::Unknown,
            rules: vec![StyloRule {
                path: vec![0],
                kind: StyloRuleKind::Style,
                native_kind: "Style".into(),
                condition: None,
                location: None,
            }],
            selectors: vec![],
            declarations: vec![],
            dependencies: vec![],
            occurrences: vec![],
            source_coverage: StyloCoverage::Partial,
            source_gaps: vec![
                StyloSourceGap::NestedOrComplexUncovered,
                StyloSourceGap::CustomPropertyReferencesUncovered,
            ],
            source_observer_recipe: "test-observer-v1".into(),
            diagnostics: vec![],
            coverage: StyloCoverage::Partial,
        };
        let worker_policy = StyloWorkerPolicy {
            wall_time_nanos: 1,
            input_bytes: 1024,
            stdout_bytes: 1024,
            stderr_bytes: 64,
            output_bytes: 2048,
            memory_bytes: None,
        };
        let basis = super::super::StyloProjectionBasis {
            project: [1; 32],
            path: "site.css",
            content: [2; 32],
            source: [4; 32],
            byte_length: 15,
            producer: [3; 32],
            worker_artifact: [7; 32],
            adapter: [8; 32],
            worker_policy: &worker_policy,
        };
        let (analysis_basis, projected) =
            super::super::project_stylo_facts(&basis, &analysis).unwrap();
        assert_eq!(projected.len(), 1);
        let host_key = stylo_host_key([1; 32], "site.css").unwrap();
        let build = |fact_key: [u8; 32]| {
            let fact = StyloAnalysisRecord::Fact {
                host: host_key,
                ordinal: projected[0].ordinal,
                subordinal: projected[0].subordinal,
                fact: projected[0].fact.clone(),
            };
            let fact_digest = stylo_rows_digest(&[(fact_key, &fact)]).unwrap();
            let host = StyloAnalysisRecord::Host {
                project: [1; 32],
                path: "site.css".into(),
                content: Some([2; 32]),
                source: Some([4; 32]),
                byte_length: Some(15),
                utf8_bom_removed: Some(false),
                config: Some(config.clone()),
                configuration: Some(
                    super::super::stylo_parser_configuration_id(&recipe, &config).unwrap(),
                ),
                preferences: Some(preferences.clone()),
                preference_stability: Some(StyloPreferenceStability::Unknown),
                analysis_basis: Some(analysis_basis),
                worker_policy: Some(worker_policy.clone()),
                source_coverage: Some(analysis.source_coverage),
                source_gaps: analysis.source_gaps.clone(),
                source_observer_recipe: Some(analysis.source_observer_recipe.clone()),
                terminal: StyloTerminal::Parsed(StyloCoverage::Partial),
                fact_count: 1,
                fact_digest,
            };
            let host_digest = stylo_rows_digest(&[(host_key, &host)]).unwrap();
            let mut witness = StyloInventoryWitness::default();
            witness.push("site.css").unwrap();
            let (inventory_count, inventory_digest) = witness.finish();
            let selection = StyloAnalysisRecord::Selection {
                project: [1; 32],
                source_root: "/project".into(),
                profile: StyloAnalysisProfile::StyloStandaloneV1,
                recipe: recipe.clone(),
                producer: [3; 32],
                worker_artifact: [7; 32],
                adapter: [8; 32],
                discovery: [5; 32],
                policy_complete: false,
                max_worker_attempts: None,
                inventory: StyloInventory::Complete,
                inventory_digest,
                inventory_count,
                host_count: 1,
                host_digest,
            };
            (selection, host, fact)
        };
        let (selection, host, fact) = build(projected[0].key);
        let mut rows = vec![
            (stylo_selection_key([1; 32]), &selection),
            (host_key, &host),
            (projected[0].key, &fact),
        ];
        rows.sort_by_key(|row| row.0);
        assert!(admit_stylo_relation(&rows).is_ok());
        let forged_key = [7; 32];
        let (selection, host, fact) = build(forged_key);
        let mut rows = vec![
            (stylo_selection_key([1; 32]), &selection),
            (host_key, &host),
            (forged_key, &fact),
        ];
        rows.sort_by_key(|row| row.0);
        assert!(admit_stylo_relation(&rows).is_err());
    }
    #[test]
    fn global_scan_rejects_a_forged_empty_selection_key() {
        let recipe = "stylo-test".to_owned();
        let (_, inventory_digest) = StyloInventoryWitness::default().finish();
        let selection = StyloAnalysisRecord::Selection {
            project: [1; 32],
            source_root: "/project".into(),
            profile: StyloAnalysisProfile::StyloStandaloneV1,
            recipe,
            producer: [2; 32],
            worker_artifact: [7; 32],
            adapter: [8; 32],
            discovery: [3; 32],
            policy_complete: false,
            max_worker_attempts: None,
            inventory: StyloInventory::Complete,
            inventory_digest,
            inventory_count: 0,
            host_count: 0,
            host_digest: stylo_rows_digest(&[]).unwrap(),
        };
        assert!(admit_stylo_relation(&[(stylo_selection_key([1; 32]), &selection)]).is_ok());
        assert!(admit_stylo_relation(&[([5; 32], &selection)]).is_err());
    }
    #[test]
    fn native_schema_rejects_legacy_rows_and_separates_pointer_version() {
        let v1 = ObjectKey::<QuarantinedV1>::from_value(&ROOT_KEY);
        let v2 = ObjectKey::<StyloAnalysisRootSchema>::from_value(&ROOT_KEY);
        assert_ne!(v1.as_bytes(), v2.as_bytes());
        let value = StyloAnalysisRecord::Host {
            project: [1; 32],
            path: "site.css".into(),
            content: None,
            source: None,
            byte_length: None,
            utf8_bom_removed: None,
            config: None,
            configuration: None,
            preferences: None,
            preference_stability: None,
            analysis_basis: None,
            worker_policy: None,
            source_coverage: None,
            source_gaps: vec![],
            source_observer_recipe: None,
            terminal: StyloTerminal::Unavailable(StyloUnavailable::ReadUnavailable),
            fact_count: 0,
            fact_digest: [2; 32],
        };
        let mut encoded = Vec::new();
        StyloAnalysisRelation::encode_value(&value, &mut encoded);
        assert_eq!(
            StyloAnalysisRelation::decode_value(&encoded).ok(),
            Some(value)
        );
        encoded[..4].copy_from_slice(b"PAR1");
        assert!(StyloAnalysisRelation::decode_value(&encoded).is_err());
    }
}

/// Validates the bounded rows belonging to one selected native project.
/// This checks canonical keys, digests, ownership, hierarchy, configuration
/// and terminal consistency. It does not prove closure membership, producer
/// execution, host bytes, or completeness of an incomplete inventory.
///
/// # Errors
/// Rejects any inconsistent selection, host, or native fact witness.
pub fn admit_stylo_selection(
    selection_key: [u8; 32],
    selection: &StyloAnalysisRecord,
    hosts: &[([u8; 32], &StyloAnalysisRecord)],
    facts: &[([u8; 32], &StyloAnalysisRecord)],
) -> Result<(), String> {
    use super::stylo_projection::{StyloProjectionBasis, project_stylo_facts};
    use backend_semantic::stylo_analysis::StyloAnalysis;
    use std::collections::BTreeMap;
    let StyloAnalysisRecord::Selection {
        project,
        recipe,
        producer,
        worker_artifact,
        adapter,
        inventory,
        inventory_digest,
        inventory_count,
        host_count,
        host_digest,
        ..
    } = selection
    else {
        return Err("Stylo selection row required".into());
    };
    selection.validate()?;
    if hosts.len() > 65_536 || facts.len() > 262_144 {
        return Err("Stylo selection exceeds row work limit".into());
    }
    if selection_key != stylo_selection_key(*project)
        || hosts.len() != *host_count as usize
        || *inventory_count < u64::from(*host_count)
        || stylo_rows_digest(hosts)? != *host_digest
    {
        return Err("Stylo selected host witness mismatch".into());
    }
    // Work is bounded before allocating lookup structures or cloning facts.
    stylo_rows_digest(facts)?;
    let mut host_paths = Vec::with_capacity(hosts.len());
    let mut by_host: BTreeMap<[u8; 32], Vec<([u8; 32], &StyloAnalysisRecord)>> = BTreeMap::new();
    for (key, record) in hosts {
        let StyloAnalysisRecord::Host {
            project: owner,
            path,
            ..
        } = record
        else {
            return Err("Stylo host row required".into());
        };
        if owner != project
            || *key != stylo_host_key(*project, path)?
            || by_host.insert(*key, Vec::new()).is_some()
        {
            return Err("invalid Stylo host ownership".into());
        }
        host_paths.push(path.as_str());
    }
    host_paths.sort_unstable();
    let mut inventory_witness = StyloInventoryWitness::default();
    for path in host_paths {
        inventory_witness.push(path)?;
    }
    let (retained_count, retained_digest) = inventory_witness.finish();
    if *inventory == StyloInventory::Complete
        && (retained_count != *inventory_count || retained_digest != *inventory_digest)
    {
        return Err("complete Stylo inventory witness mismatch".into());
    }
    for (key, record) in facts {
        let StyloAnalysisRecord::Fact { host, .. } = record else {
            return Err("Stylo fact row required".into());
        };
        by_host
            .get_mut(host)
            .ok_or("Stylo fact has no selected host")?
            .push((*key, *record));
    }
    for (host_key, record) in hosts {
        let StyloAnalysisRecord::Host {
            path,
            content,
            source,
            byte_length,
            utf8_bom_removed,
            config,
            configuration,
            preferences,
            preference_stability,
            analysis_basis,
            worker_policy,
            source_coverage,
            source_gaps,
            source_observer_recipe,
            terminal,
            fact_count,
            fact_digest,
            ..
        } = record
        else {
            return Err("Stylo host row required".into());
        };
        let selected = by_host
            .remove(host_key)
            .ok_or("Stylo host witness missing")?;
        if selected.len() != *fact_count as usize || stylo_rows_digest(&selected)? != *fact_digest {
            return Err("Stylo host fact witness mismatch".into());
        }
        if let Some(config) = config {
            let expected = super::stylo_projection::stylo_parser_configuration_id(recipe, config)?;
            if *configuration != Some(expected) {
                return Err("Stylo attempted parser configuration mismatch".into());
            }
        }
        let StyloTerminal::Parsed(coverage) = terminal else {
            if !selected.is_empty() {
                return Err("unavailable Stylo host contains facts".into());
            }
            continue;
        };
        let mut rules = Vec::new();
        let mut selectors = Vec::new();
        let mut declarations = Vec::new();
        let mut dependencies = Vec::new();
        let mut occurrences = Vec::new();
        let mut diagnostics = Vec::new();
        for (_, record) in &selected {
            let StyloAnalysisRecord::Fact {
                ordinal,
                subordinal,
                fact,
                ..
            } = record
            else {
                return Err("Stylo fact row required".into());
            };
            match fact {
                StyloProjectedRow::Rule(value) => {
                    rules.push((value.path.clone(), *ordinal, *subordinal, value.clone()))
                }
                StyloProjectedRow::Selector(value) => selectors.push((
                    value.rule_path.clone(),
                    *ordinal,
                    *subordinal,
                    value.clone(),
                )),
                StyloProjectedRow::RetainedDeclaration(value) => declarations.push((
                    value.rule_path.clone(),
                    *ordinal,
                    *subordinal,
                    value.clone(),
                )),
                StyloProjectedRow::Dependency(value) => dependencies.push((
                    value.rule_path.clone(),
                    *ordinal,
                    *subordinal,
                    value.clone(),
                )),
                StyloProjectedRow::SourceOccurrence(value) => {
                    occurrences.push((*ordinal, *subordinal, value.clone()))
                }
                StyloProjectedRow::Diagnostic(value) => {
                    diagnostics.push((*ordinal, *subordinal, value.clone()))
                }
            }
        }
        rules.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
        selectors.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
        declarations.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
        dependencies.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
        occurrences.sort_by_key(|entry| (entry.0, entry.1));
        diagnostics.sort_by_key(|entry| (entry.0, entry.1));
        let host_config = config
            .clone()
            .ok_or("Stylo host parser configuration missing")?;
        if Some(super::stylo_projection::stylo_parser_configuration_id(
            recipe,
            &host_config,
        )?) != *configuration
        {
            return Err("Stylo host configuration commitment mismatch".into());
        }
        let analysis = StyloAnalysis {
            config: host_config,
            recipe: recipe.clone(),
            support_fingerprint: *producer,
            utf8_bom_removed: utf8_bom_removed.ok_or("Stylo BOM handling missing")?,
            preferences: preferences
                .clone()
                .ok_or("Stylo preference snapshot missing")?,
            preference_stability: preference_stability
                .ok_or("Stylo preference stability missing")?,
            rules: rules.into_iter().map(|entry| entry.3).collect(),
            selectors: selectors.into_iter().map(|entry| entry.3).collect(),
            declarations: declarations.into_iter().map(|entry| entry.3).collect(),
            dependencies: dependencies.into_iter().map(|entry| entry.3).collect(),
            occurrences: occurrences.into_iter().map(|entry| entry.2).collect(),
            source_coverage: source_coverage.ok_or("Stylo source coverage missing")?,
            source_gaps: source_gaps.clone(),
            source_observer_recipe: source_observer_recipe
                .clone()
                .ok_or("Stylo source observer recipe missing")?,
            diagnostics: diagnostics.into_iter().map(|entry| entry.2).collect(),
            coverage: *coverage,
        };
        let basis = StyloProjectionBasis {
            project: *project,
            path,
            content: content.ok_or("Stylo content missing")?,
            source: source.ok_or("Stylo source identity missing")?,
            byte_length: byte_length.ok_or("Stylo byte length missing")?,
            producer: *producer,
            worker_artifact: *worker_artifact,
            adapter: *adapter,
            worker_policy: worker_policy
                .as_ref()
                .ok_or("Stylo worker policy missing")?,
        };
        let (computed_basis, projected) = project_stylo_facts(&basis, &analysis)?;
        if Some(computed_basis) != *analysis_basis || projected.len() != selected.len() {
            return Err("Stylo fact basis mismatch".into());
        }
        for (projected, (key, record)) in projected.iter().zip(&selected) {
            let StyloAnalysisRecord::Fact {
                host,
                ordinal,
                subordinal,
                fact,
            } = record
            else {
                return Err("Stylo fact row required".into());
            };
            if projected.key != *key
                || *host != *host_key
                || projected.ordinal != *ordinal
                || projected.subordinal != *subordinal
                || projected.fact != *fact
            {
                return Err("Stylo fact key or payload mismatch".into());
            }
        }
    }
    Ok(())
}

/// Validates a complete, bounded scan of the selected native relation.
/// Every selection, host and fact must be owned by exactly one selected
/// project; a partial page is not sufficient input. This authenticates neither
/// closure membership nor the historical producer execution.
///
/// # Errors
/// Rejects unordered, excessive, unowned or inconsistent selected rows.
pub fn admit_stylo_relation(rows: &[([u8; 32], &StyloAnalysisRecord)]) -> Result<(), String> {
    use std::collections::BTreeMap;
    if rows.len() > 262_144 {
        return Err("Stylo relation exceeds scan work limit".into());
    }
    stylo_rows_digest(rows)?;
    type ProjectRows<'a> = (
        &'a StyloAnalysisRecord,
        Vec<([u8; 32], &'a StyloAnalysisRecord)>,
        Vec<([u8; 32], &'a StyloAnalysisRecord)>,
    );
    let mut projects: BTreeMap<[u8; 32], ProjectRows<'_>> = BTreeMap::new();
    for (key, row) in rows {
        if let StyloAnalysisRecord::Selection { project, .. } = row {
            if *key != stylo_selection_key(*project)
                || projects
                    .insert(*project, (row, Vec::new(), Vec::new()))
                    .is_some()
            {
                return Err("noncanonical or duplicate Stylo selection".into());
            }
        }
    }
    let mut attempts = BTreeMap::new();
    for (key, row) in rows {
        if let StyloAnalysisRecord::Attempt {
            project,
            served_selection,
            outcome,
            source_root,
            discovery,
            policy_complete,
            max_worker_attempts,
            inventory,
            inventory_digest,
            inventory_count,
            expected_producer,
            expected_recipe,
            adapter,
            worker_artifact,
            ..
        } = row
        {
            if *key != stylo_attempt_key(*project) || attempts.insert(*project, *row).is_some() {
                return Err("noncanonical or duplicate Stylo latest attempt".into());
            }
            let selection = projects.get(project).map(|(selection, _, _)| *selection);
            let linked = selection
                .map(|value| stylo_rows_digest(&[(stylo_selection_key(*project), value)]))
                .transpose()?;
            if *served_selection != linked {
                return Err("Stylo latest attempt selection link mismatch".into());
            }
            if matches!(outcome, StyloAttemptOutcome::Observed) {
                let Some(StyloAnalysisRecord::Selection {
                    source_root: old_root,
                    discovery: old_discovery,
                    policy_complete: old_policy_complete,
                    max_worker_attempts: old_max_attempts,
                    inventory: old_inventory,
                    inventory_digest: old_digest,
                    inventory_count: old_count,
                    producer,
                    recipe,
                    adapter: old_adapter,
                    worker_artifact: old_artifact,
                    ..
                }) = selection
                else {
                    return Err("observed Stylo attempt requires a selection".into());
                };
                if source_root != old_root
                    || *discovery != Some(*old_discovery)
                    || *policy_complete != *old_policy_complete
                    || *old_max_attempts != *max_worker_attempts
                    || *inventory != Some(*old_inventory)
                    || *inventory_digest != Some(*old_digest)
                    || *inventory_count != Some(*old_count)
                    || *expected_producer != Some(*producer)
                    || expected_recipe.as_ref() != Some(recipe)
                    || *adapter != *old_adapter
                    || *worker_artifact != StyloAttemptArtifact::Admitted(*old_artifact)
                {
                    return Err("observed Stylo attempt differs from selection".into());
                }
            }
        }
    }
    let mut owners = BTreeMap::new();
    for (key, row) in rows {
        if let StyloAnalysisRecord::Host { project, path, .. } = row {
            if *key != stylo_host_key(*project, path)? || owners.insert(*key, *project).is_some() {
                return Err("noncanonical or duplicate Stylo host".into());
            }
            projects
                .get_mut(project)
                .ok_or("Stylo host without selection")?
                .1
                .push((*key, *row));
        }
    }
    for (key, row) in rows {
        if let StyloAnalysisRecord::Fact { host, .. } = row {
            let project = owners.get(host).ok_or("Stylo fact without host")?;
            projects
                .get_mut(project)
                .ok_or("Stylo fact without selection")?
                .2
                .push((*key, *row));
        }
    }
    for (project, row) in &attempts {
        if let StyloAnalysisRecord::Attempt {
            outcome: StyloAttemptOutcome::Observed,
            requested_policy,
            ..
        } = row
        {
            if let Some((_, hosts, _)) = projects.get(project) {
                for (_, host) in hosts {
                    if let StyloAnalysisRecord::Host {
                        worker_policy: Some(policy),
                        ..
                    } = host
                        && policy != requested_policy
                    {
                        return Err(
                            "observed Stylo attempt process policy differs from host".into()
                        );
                    }
                }
            }
        }
    }
    for (project, (selection, hosts, facts)) in projects {
        admit_stylo_selection(stylo_selection_key(project), selection, &hosts, &facts)?;
    }
    Ok(())
}
