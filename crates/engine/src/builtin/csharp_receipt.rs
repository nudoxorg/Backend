//! Canonical C# captured-program receipt claims.
//!
//! A decoded row is an untrusted claim, not a source/read/foreign capability.
//! An owner must bind the complete row set to the selected compiler capture,
//! unit, exact input/configuration policy, source bytes, and image closure.
use backend_store::TypedObject;
use backend_version::{
    CanonicalRelation, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity, StateRoot,
};
use serde::{Deserialize, Serialize};

use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};

const MAGIC: &[u8; 4] = b"CSR1";
const ROOT_KEY: [u8; 32] = [0x63; 32];
/// Maximum canonical bytes of one receipt row.
pub const MAX_CSHARP_RECEIPT_ROW_BYTES: usize = 16 * 1024;
/// Maximum source members admitted by one bounded receipt.
pub const MAX_CSHARP_RECEIPT_MEMBERS: usize = 8192;

/// C# captured-unit receipt claim relation, separately versioned from TSZ.
#[derive(Debug)]
pub struct CSharpReceiptRelation;
/// Typed root pointer for the C# receipt relation.
pub struct CSharpReceiptRootSchema;
impl Schema for CSharpReceiptRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 21;
    const VERSION: u8 = 1;
    type Value = [u8; 32];
    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// An exact compiler-selected C# unit; package root and project are distinct.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CSharpReceiptUnit {
    /// Explicit package-root compilation unit.
    PackageRoot,
    /// One exact package-relative project file.
    Project { path: String },
}

/// Coverage of unproven project inputs; zero diagnostics do not upgrade it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CSharpInputCoverage {
    /// An input class was observed but is not known complete.
    Partial,
    /// Completeness has not been established.
    Unknown,
}

/// One input class observation; unknown is distinct from known inapplicability.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CSharpInputObservation {
    Observed { digest: [u8; 32] },
    NotApplicable,
    Unavailable,
    Unknown,
}
impl CSharpInputObservation {
    fn valid(&self) -> bool {
        match self {
            Self::Observed { digest } => nonzero(digest),
            _ => true,
        }
    }
}

/// A source's explicit terminal. None of these values grant serving authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CSharpMemberOutcome {
    Published {
        image: CSharpReceiptImage,
        invocation: [u8; 32],
    },
    Unavailable,
    Unknown,
}

/// One selected semantic image claim; byte identity must be rechecked by its owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CSharpReceiptImage {
    /// ArtifactId<IrSemanticImageEncoding, IrSemanticImageDomain> of canonical image bytes.
    pub identity: [u8; 32],
    /// Exact encoded image length.
    pub byte_length: u64,
}

/// One unit claim or captured member claim. Neither is independently trusted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CSharpReceiptRecord {
    /// Unit inputs and the commitment to its ordered member rows.
    Unit {
        project: [u8; 32],
        unit: CSharpReceiptUnit,
        /// Exact CompilationTargetDomain identity, including the selected unit.
        compiler_target: [u8; 32],
        source_root: [u8; 32],
        /// Canonical exact immutable compiler publication key and claim.
        publication_key: Vec<u8>,
        publication_claim: Vec<u8>,
        /// Exact c00c evidence object and extent in the selected compiler closure.
        evidence_object: [u8; 32],
        evidence_length: u32,
        input_digest: [u8; 32],
        producer_policy: [u8; 32],
        toolchain: [u8; 32],
        helper: [u8; 32],
        config_inputs: CSharpInputObservation,
        reference_inputs: CSharpInputObservation,
        sdk_inputs: CSharpInputObservation,
        input_coverage: CSharpInputCoverage,
        member_count: u32,
        members_digest: [u8; 32],
    },
    /// Exact source bytes and, when produced, its semantic image.
    Member {
        project: [u8; 32],
        unit: CSharpReceiptUnit,
        path: String,
        /// SourceFactDomain content identity of the exact source bytes.
        source_identity: [u8; 32],
        /// InputContentSchema object version of the same captured bytes.
        input_content: [u8; 32],
        /// Raw SHA-256 of those same bytes, matching the Roslyn NCAI binding.
        content_digest: [u8; 32],
        byte_length: u64,
        outcome: CSharpMemberOutcome,
    },
}

fn path_valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.contains('\\')
        && !(path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic())
        && !path.bytes().any(|byte| byte.is_ascii_control())
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
fn unit_valid(unit: &CSharpReceiptUnit) -> bool {
    match unit {
        CSharpReceiptUnit::PackageRoot => true,
        CSharpReceiptUnit::Project { path } => {
            path_valid(path) && path.to_ascii_lowercase().ends_with(".csproj")
        }
    }
}
fn nonzero(id: &[u8; 32]) -> bool {
    *id != [0; 32]
}

impl CSharpReceiptRecord {
    /// Checks bounded local shape only; this does not authenticate the claim.
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        match self {
            Self::Unit {
                project,
                unit,
                compiler_target,
                source_root,
                publication_key,
                publication_claim,
                evidence_object,
                evidence_length,
                input_digest,
                producer_policy,
                toolchain,
                helper,
                config_inputs,
                reference_inputs,
                sdk_inputs,
                input_coverage,
                member_count,
                members_digest,
                ..
            } => {
                if !nonzero(project)
                    || !unit_valid(unit)
                    || !nonzero(compiler_target)
                    || !nonzero(source_root)
                    || publication_key.is_empty()
                    || publication_key.len() > 2048
                    || publication_claim.is_empty()
                    || publication_claim.len() > 1024
                    || !nonzero(evidence_object)
                    || *evidence_length == 0
                    || *evidence_length as usize
                        > backend_semantic::ir::MAX_CSHARP_INVOCATION_EVIDENCE_BYTES
                    || !nonzero(input_digest)
                    || !nonzero(producer_policy)
                    || !nonzero(toolchain)
                    || !nonzero(helper)
                    || !config_inputs.valid()
                    || !reference_inputs.valid()
                    || !sdk_inputs.valid()
                    || (*input_coverage == CSharpInputCoverage::Partial
                        && ![config_inputs, reference_inputs, sdk_inputs].iter().any(
                            |observation| {
                                matches!(observation, CSharpInputObservation::Observed { .. })
                            },
                        ))
                    || *member_count == 0
                    || *member_count as usize > MAX_CSHARP_RECEIPT_MEMBERS
                    || !nonzero(members_digest)
                {
                    return Err("invalid C# unit receipt");
                }
            }
            Self::Member {
                project,
                unit,
                path,
                source_identity,
                input_content,
                content_digest,
                byte_length,
                outcome,
            } => {
                if !nonzero(project)
                    || !unit_valid(unit)
                    || !path_valid(path)
                    || !path.to_ascii_lowercase().ends_with(".cs")
                    || !nonzero(source_identity)
                    || !nonzero(input_content)
                    || !nonzero(content_digest)
                    || *byte_length > 16 * 1024 * 1024
                    || match outcome {
                        CSharpMemberOutcome::Published { image, invocation } => {
                            !nonzero(invocation)
                                || !nonzero(&image.identity)
                                || image.byte_length == 0
                                || image.byte_length > 32 * 1024 * 1024
                        }
                        CSharpMemberOutcome::Unavailable | CSharpMemberOutcome::Unknown => false,
                    }
                {
                    return Err("invalid C# member receipt");
                }
            }
        }
        let encoded = serde_json::to_vec(self).map_err(|_| "invalid C# receipt encoding")?;
        if encoded.len().saturating_add(MAGIC.len()) > MAX_CSHARP_RECEIPT_ROW_BYTES {
            return Err("C# receipt row too large");
        }
        Ok(())
    }
}

/// Derives the exact relation key; the owner must compare it with the stored key.
pub fn csharp_receipt_key(record: &CSharpReceiptRecord) -> Result<[u8; 32], &'static str> {
    record.validate_shape()?;
    let (project, unit, path) = match record {
        CSharpReceiptRecord::Unit { project, unit, .. } => (project, unit, None),
        CSharpReceiptRecord::Member {
            project,
            unit,
            path,
            ..
        } => (project, unit, Some(path.as_str())),
    };
    let unit_bytes = serde_json::to_vec(unit).map_err(|_| "invalid C# unit")?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.csharp-captured-unit.key.v1\0");
    hash.update(project);
    hash.update(&(unit_bytes.len() as u64).to_be_bytes());
    hash.update(&unit_bytes);
    match path {
        None => {
            hash.update(&[0]);
        }
        Some(path) => {
            hash.update(&[1]);
            hash.update(&(path.len() as u64).to_be_bytes());
            hash.update(path.as_bytes());
        }
    }
    Ok(*hash.finalize().as_bytes())
}

impl Relation for CSharpReceiptRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 20;
    const VERSION: u8 = 1;
    type Key = [u8; 32];
    type Value = CSharpReceiptRecord;
    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&serde_json::to_vec(value).expect("serializable C# receipt"));
    }
}
impl CanonicalRelation for CSharpReceiptRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes.try_into().map_err(|_| RelationDecodeError::Malformed)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        if bytes.len() > MAX_CSHARP_RECEIPT_ROW_BYTES || !bytes.starts_with(MAGIC) {
            return Err(RelationDecodeError::Malformed);
        }
        let value: CSharpReceiptRecord = serde_json::from_slice(&bytes[MAGIC.len()..])
            .map_err(|_| RelationDecodeError::Malformed)?;
        value
            .validate_shape()
            .map_err(|_| RelationDecodeError::Malformed)?;
        let mut canonical = Vec::new();
        Self::encode_value(&value, &mut canonical);
        if canonical != bytes {
            return Err(RelationDecodeError::Malformed);
        }
        Ok(value)
    }
}

/// Checks the stored key and decoded row together, without granting authority.
pub fn admit_csharp_receipt_row(
    key: &[u8; 32],
    row: &CSharpReceiptRecord,
) -> Result<(), &'static str> {
    if csharp_receipt_key(row)? != *key {
        return Err("C# receipt key does not match row");
    }
    Ok(())
}

/// Checks a complete bounded relation's framing and unit/member ownership.
/// This does not establish that any compiler publication is selected.
pub fn admit_csharp_receipt_relation(
    rows: &[([u8; 32], &CSharpReceiptRecord)],
) -> Result<(), &'static str> {
    admit_csharp_receipt_relation_checked(rows, &mut || Ok(()))
}

/// Cancellable form for bounded owner planning; it grants no authority.
pub fn admit_csharp_receipt_relation_cancellable(
    rows: &[([u8; 32], &CSharpReceiptRecord)],
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<(), &'static str> {
    admit_csharp_receipt_relation_checked(rows, &mut || {
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            Err("C# receipt validation cancelled")
        } else {
            Ok(())
        }
    })
}

fn admit_csharp_receipt_relation_checked(
    rows: &[([u8; 32], &CSharpReceiptRecord)],
    check: &mut impl FnMut() -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    check()?;
    if rows.len() > 16384 {
        return Err("C# receipt relation exceeds row budget");
    }
    let mut units = std::collections::BTreeMap::new();
    let mut members =
        std::collections::BTreeMap::<[u8; 32], Vec<([u8; 32], &CSharpReceiptRecord)>>::new();
    let mut prior = None;
    let mut total = 0usize;
    for (key, record) in rows {
        check()?;
        if prior.is_some_and(|previous| previous >= *key) {
            return Err("C# receipt relation not ordered");
        }
        admit_csharp_receipt_row(key, record)?;
        let encoded = serde_json::to_vec(record).map_err(|_| "invalid C# receipt encoding")?;
        total = total
            .checked_add(encoded.len() + MAGIC.len() + 32 + 8)
            .ok_or("C# receipt size overflow")?;
        if total > 32 * 1024 * 1024 {
            return Err("C# receipt relation exceeds byte budget");
        }
        match record {
            CSharpReceiptRecord::Unit { .. } => {
                units.insert(*key, *record);
            }
            CSharpReceiptRecord::Member { project, unit, .. } => {
                // The unit key depends only on project/unit, not the other unit fields.
                let unit_key = csharp_receipt_unit_key(project, unit)?;
                members.entry(unit_key).or_default().push((*key, *record));
            }
        }
        prior = Some(*key);
    }
    for (key, unit) in units {
        check()?;
        let owned = members
            .remove(&key)
            .ok_or("C# receipt unit has no members")?;
        admit_csharp_receipt_members_checked(unit, &owned, check)?;
    }
    if !members.is_empty() {
        return Err("C# receipt has orphan members");
    }
    Ok(())
}

fn csharp_receipt_unit_key(
    project: &[u8; 32],
    unit: &CSharpReceiptUnit,
) -> Result<[u8; 32], &'static str> {
    if !nonzero(project) || !unit_valid(unit) {
        return Err("invalid C# unit key");
    }
    let bytes = serde_json::to_vec(unit).map_err(|_| "invalid C# unit")?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.csharp-captured-unit.key.v1\0");
    hash.update(project);
    hash.update(&(bytes.len() as u64).to_be_bytes());
    hash.update(&bytes);
    hash.update(&[0]);
    Ok(*hash.finalize().as_bytes())
}

/// Decodes a unit's exact immutable publication claim; the owner must still
/// prove that this pair is selected and reopen its authenticated closure.
pub fn csharp_receipt_publication(
    unit: &CSharpReceiptRecord,
) -> Result<
    (
        super::ProductSemanticPublicationKey,
        super::ProductSemanticPublicationRecord,
    ),
    &'static str,
> {
    let CSharpReceiptRecord::Unit {
        project,
        unit: receipt_unit,
        compiler_target,
        publication_key,
        publication_claim,
        ..
    } = unit
    else {
        return Err("expected C# unit receipt");
    };
    unit.validate_shape()?;
    let key = super::ProductSemanticPublicationRelation::decode_key(publication_key)
        .map_err(|_| "invalid C# compiler publication key")?;
    let record = super::ProductSemanticPublicationRelation::decode_value(publication_claim)
        .map_err(|_| "invalid C# compiler publication claim")?;
    if key.package_key().as_bytes() != project
        || !matches!(
            key.profile(),
            backend_semantic::vocabulary::LanguageProfile::CSharp(_)
        )
        || key.is_selected()
        || key.admit_record(&record).is_err()
    {
        return Err("C# receipt requires matching immutable publication");
    }
    let unit_key = match receipt_unit {
        CSharpReceiptUnit::PackageRoot => {
            crate::compiler_input_manifest_v2::CompilationUnitKeyV2::PackageRoot
        }
        CSharpReceiptUnit::Project { path } => {
            crate::compiler_input_manifest_v2::CompilationUnitKeyV2::CSharpProject {
                project_path: path.as_str().into(),
            }
        }
    };
    let target = crate::compiler_input_manifest_v2::CompilerPackageTargetV2::new(
        key.coordinate().clone(),
        unit_key,
    )
    .map_err(|_| "invalid C# compiler target")?;
    if target.target().as_ref() != compiler_target {
        return Err("C# compiler target differs from receipt unit");
    }
    let mut canonical_key = Vec::new();
    let mut canonical_record = Vec::new();
    super::ProductSemanticPublicationRelation::encode_key(&key, &mut canonical_key);
    super::ProductSemanticPublicationRelation::encode_value(&record, &mut canonical_record);
    if canonical_key != *publication_key || canonical_record != *publication_claim {
        return Err("noncanonical C# compiler publication");
    }
    Ok((key, record))
}

/// Compares input-content, source-fact and Roslyn SHA-256 against one byte buffer. This does not prove
/// capture membership, a selected image, path confinement, or freshness.
pub fn csharp_receipt_matches_source(row: &CSharpReceiptRecord, bytes: &[u8]) -> bool {
    use sha2::{Digest, Sha256};
    let CSharpReceiptRecord::Member {
        source_identity,
        input_content,
        content_digest,
        byte_length,
        ..
    } = row
    else {
        return false;
    };
    let Some(source) = backend_semantic::ir::SourceIdentity::from_bytes(bytes) else {
        return false;
    };
    let Ok(verified) = backend_compile::VerifiedSourceText::capture(bytes) else {
        return false;
    };
    u64::from(source.byte_len) == *byte_length
        && verified.content().to_bytes() == *input_content
        && source.identity.as_ref() == source_identity
        && Sha256::digest(bytes).as_slice() == content_digest
}

/// Checks an ordered complete member set against its unit claim. This is only
/// structural validation; selection and source/image authority remain separate.
pub fn admit_csharp_receipt_members(
    unit: &CSharpReceiptRecord,
    members: &[([u8; 32], &CSharpReceiptRecord)],
) -> Result<(), &'static str> {
    admit_csharp_receipt_members_checked(unit, members, &mut || Ok(()))
}

fn admit_csharp_receipt_members_checked(
    unit: &CSharpReceiptRecord,
    members: &[([u8; 32], &CSharpReceiptRecord)],
    check: &mut impl FnMut() -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    check()?;
    unit.validate_shape()?;
    let CSharpReceiptRecord::Unit {
        project,
        unit: expected_unit,
        member_count,
        members_digest,
        ..
    } = unit
    else {
        return Err("expected C# unit receipt");
    };
    if members.len() != *member_count as usize {
        return Err("C# receipt member count mismatch");
    }
    if csharp_receipt_members_digest_checked(members, check)? != *members_digest {
        return Err("C# receipt member digest mismatch");
    }
    for (_, member) in members {
        check()?;
        match member {
            CSharpReceiptRecord::Member {
                project: member_project,
                unit: member_unit,
                ..
            } if member_project == project && member_unit == expected_unit => {}
            _ => return Err("C# receipt member unit mismatch"),
        }
    }
    Ok(())
}

/// Commits to bounded, strictly key-ordered member claims. The work budget
/// includes each 32-byte key, 8-byte length, and encoded value.
pub fn csharp_receipt_members_digest(
    members: &[([u8; 32], &CSharpReceiptRecord)],
) -> Result<[u8; 32], &'static str> {
    csharp_receipt_members_digest_checked(members, &mut || Ok(()))
}

fn csharp_receipt_members_digest_checked(
    members: &[([u8; 32], &CSharpReceiptRecord)],
    check: &mut impl FnMut() -> Result<(), &'static str>,
) -> Result<[u8; 32], &'static str> {
    check()?;
    if members.is_empty() || members.len() > MAX_CSHARP_RECEIPT_MEMBERS {
        return Err("invalid C# receipt member count");
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.csharp-captured-unit.members.v1\0");
    let mut prior = None;
    let mut total = 0usize;
    for (key, record) in members {
        check()?;
        if prior.is_some_and(|previous| previous >= *key) {
            return Err("C# receipt members not ordered");
        }
        if !matches!(record, CSharpReceiptRecord::Member { .. }) {
            return Err("expected C# receipt member");
        }
        admit_csharp_receipt_row(key, record)?;
        let mut bytes = Vec::new();
        CSharpReceiptRelation::encode_value(record, &mut bytes);
        if bytes.len() > MAX_CSHARP_RECEIPT_ROW_BYTES {
            return Err("C# receipt row too large");
        }
        total = total
            .checked_add(bytes.len() + 32 + 8)
            .ok_or("C# receipt size overflow")?;
        if total > 32 * 1024 * 1024 {
            return Err("C# receipt set too large");
        }
        hash.update(key);
        hash.update(&(bytes.len() as u64).to_be_bytes());
        hash.update(&bytes);
        prior = Some(*key);
    }
    hash.update(&(members.len() as u64).to_be_bytes());
    Ok(*hash.finalize().as_bytes())
}

/// Canonical key of the selected C# receipt root pointer.
#[must_use]
pub fn csharp_receipt_root_pointer_key() -> [u8; 32] {
    *ObjectKey::<CSharpReceiptRootSchema>::from_value(&ROOT_KEY).as_bytes()
}
/// Constructs a typed pointer to a C# receipt relation root.
pub fn csharp_receipt_root_object(root: StateRoot<CSharpReceiptRelation>) -> TypedObject {
    let key = ObjectKey::<CSharpReceiptRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}
/// Opens the selected relation; absence is unknown, never an empty proof.
pub fn csharp_receipt_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceRelationHandle<CSharpReceiptRelation>>, WorkspaceRelationError> {
    let key = ObjectKey::<CSharpReceiptRootSchema>::from_value(&ROOT_KEY);
    snapshot.auxiliary_relation::<CSharpReceiptRelation>(
        SchemaIdentity::new(
            CSharpReceiptRootSchema::DOMAIN,
            CSharpReceiptRootSchema::TYPE,
            CSharpReceiptRootSchema::VERSION,
        ),
        *key.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(path: &str) -> CSharpReceiptRecord {
        CSharpReceiptRecord::Member {
            project: [1; 32],
            unit: CSharpReceiptUnit::Project {
                path: "App/App.csproj".into(),
            },
            path: path.into(),
            source_identity: [2; 32],
            input_content: [14; 32],
            content_digest: [3; 32],
            byte_length: 10,
            outcome: CSharpMemberOutcome::Published {
                invocation: [12; 32],
                image: CSharpReceiptImage {
                    identity: [4; 32],
                    byte_length: 40,
                },
            },
        }
    }

    #[test]
    fn complete_set_requires_exact_ordered_unit_members() {
        let first = member("App/A.cs");
        let second = member("App/B.cs");
        let mut rows = [
            (csharp_receipt_key(&first).unwrap(), &first),
            (csharp_receipt_key(&second).unwrap(), &second),
        ];
        rows.sort_by_key(|row| row.0);
        let mut unit = CSharpReceiptRecord::Unit {
            project: [1; 32],
            unit: CSharpReceiptUnit::Project {
                path: "App/App.csproj".into(),
            },
            compiler_target: [13; 32],
            source_root: [5; 32],
            publication_key: vec![1],
            publication_claim: vec![2],
            evidence_object: [15; 32],
            evidence_length: 100,
            input_digest: [6; 32],
            producer_policy: [7; 32],
            toolchain: [8; 32],
            helper: [9; 32],
            config_inputs: CSharpInputObservation::Unknown,
            reference_inputs: CSharpInputObservation::Unknown,
            sdk_inputs: CSharpInputObservation::Unknown,
            input_coverage: CSharpInputCoverage::Unknown,
            member_count: 2,
            members_digest: csharp_receipt_members_digest(&rows).unwrap(),
        };
        assert!(admit_csharp_receipt_members(&unit, &rows).is_ok());
        let mut inconsistent = unit.clone();
        if let CSharpReceiptRecord::Unit { input_coverage, .. } = &mut inconsistent {
            *input_coverage = CSharpInputCoverage::Partial;
        }
        assert!(inconsistent.validate_shape().is_err());
        let unit_key = csharp_receipt_key(&unit).unwrap();
        let mut relation = vec![rows[0], rows[1], (unit_key, &unit)];
        relation.sort_by_key(|row| row.0);
        assert!(admit_csharp_receipt_relation(&relation).is_ok());
        assert!(admit_csharp_receipt_relation(&rows).is_err());
        // Structural membership never turns arbitrary publication bytes into authority.
        assert!(csharp_receipt_publication(&unit).is_err());
        assert!(admit_csharp_receipt_members(&unit, &rows[..1]).is_err());
        rows.swap(0, 1);
        assert!(admit_csharp_receipt_members(&unit, &rows).is_err());
        rows.swap(0, 1);
        if let CSharpReceiptRecord::Unit { project, .. } = &mut unit {
            *project = [11; 32];
        }
        assert!(admit_csharp_receipt_members(&unit, &rows).is_err());
    }

    #[test]
    fn publication_must_match_package_profile_and_exact_unit_target() {
        use super::super::{
            ProductSemanticPublicationKey, ProductSemanticPublicationRecord,
            ProductSemanticPublicationRelation, SemanticPublicationClaim,
            SemanticPublicationCoverage,
        };
        use crate::publication::binding::{COMPILATION_BINDING_BYTES, CompilationBindingView};
        use crate::publication::manifest::{CompilationManifestFacts, CompilationManifestFormat};
        use backend_library::PackageReference;
        use backend_semantic::vocabulary::{CSharpVersion, LanguageProfile, PackageUrl};
        use backend_store::hydration::VerifiedGenerationFacts;
        use backend_version::{
            ArtifactId, ContentId, GenerationId, IrManifestDomain, IrManifestEncoding,
        };
        let coordinate = PackageUrl::parse("pkg:nuget/example@1.0.0".to_owned()).unwrap();
        let selected = ProductSemanticPublicationKey::new(
            PackageReference::parse("pkg:nuget/example@1.0.0".to_owned()).unwrap(),
            coordinate.clone(),
            LanguageProfile::CSharp(CSharpVersion::CSharp14),
        )
        .unwrap();
        let generation = VerifiedGenerationFacts {
            pinned_root: GenerationId::from_canonical_bytes(b"generation"),
            dep_set: ContentId::from_canonical_bytes(b"deps"),
        };
        let manifest =
            ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(b"manifest");
        let mut binding_bytes = [0; COMPILATION_BINDING_BYTES];
        let binding =
            *CompilationBindingView::write_into(generation, manifest, &mut binding_bytes).unwrap();
        let claim = SemanticPublicationClaim::admit(
            CompilationManifestFacts {
                identity: manifest,
                format: CompilationManifestFormat::SemanticV2,
                fragment_count: 1,
                byte_length: 100,
            },
            binding,
        )
        .unwrap();
        let key = selected.for_generation(claim.binding().identity);
        let record = ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Complete,
            claim,
        };
        let mut publication_key = Vec::new();
        let mut publication_claim = Vec::new();
        ProductSemanticPublicationRelation::encode_key(&key, &mut publication_key);
        ProductSemanticPublicationRelation::encode_value(&record, &mut publication_claim);
        let unit = CSharpReceiptUnit::Project {
            path: "App/App.csproj".into(),
        };
        let target = crate::compiler_input_manifest_v2::CompilerPackageTargetV2::new(
            coordinate,
            crate::compiler_input_manifest_v2::CompilationUnitKeyV2::CSharpProject {
                project_path: "App/App.csproj".into(),
            },
        )
        .unwrap();
        let mut receipt = CSharpReceiptRecord::Unit {
            project: *key.package_key().as_bytes(),
            unit,
            compiler_target: *target.target().as_ref(),
            source_root: [1; 32],
            publication_key,
            publication_claim,
            evidence_object: [15; 32],
            evidence_length: 100,
            input_digest: [2; 32],
            producer_policy: [3; 32],
            toolchain: [4; 32],
            helper: [5; 32],
            config_inputs: CSharpInputObservation::Unknown,
            reference_inputs: CSharpInputObservation::Unknown,
            sdk_inputs: CSharpInputObservation::Unknown,
            input_coverage: CSharpInputCoverage::Unknown,
            member_count: 1,
            members_digest: [6; 32],
        };
        assert!(csharp_receipt_publication(&receipt).is_ok());
        if let CSharpReceiptRecord::Unit {
            compiler_target, ..
        } = &mut receipt
        {
            *compiler_target = [9; 32];
        }
        assert!(csharp_receipt_publication(&receipt).is_err());
    }

    #[test]
    fn receipt_validation_observes_cancellation_even_for_empty_relation() {
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert!(admit_csharp_receipt_relation_cancellable(&[], &cancelled).is_err());
        cancelled.store(false, std::sync::atomic::Ordering::Release);
        assert!(admit_csharp_receipt_relation_cancellable(&[], &cancelled).is_ok());
    }

    #[test]
    fn malformed_rows_and_forged_keys_are_refused() {
        let record = member("App/A.cs");
        assert!(!csharp_receipt_matches_source(&record, b"different"));
        let mut bound = record.clone();
        let bytes = b"same bytes";
        if let CSharpReceiptRecord::Member {
            source_identity,
            input_content,
            content_digest,
            byte_length,
            ..
        } = &mut bound
        {
            use sha2::{Digest, Sha256};
            *source_identity = *backend_semantic::ir::SourceIdentity::from_bytes(bytes)
                .unwrap()
                .identity
                .as_ref();
            *input_content = backend_compile::VerifiedSourceText::capture(bytes)
                .unwrap()
                .content()
                .to_bytes();
            *content_digest = Sha256::digest(bytes).into();
            *byte_length = bytes.len() as u64;
        }
        assert!(csharp_receipt_matches_source(&bound, bytes));
        assert!(!csharp_receipt_matches_source(&bound, b"same byte!"));
        if let CSharpReceiptRecord::Member { input_content, .. } = &mut bound {
            *input_content = [8; 32];
        }
        assert!(!csharp_receipt_matches_source(&bound, bytes));
        let key = csharp_receipt_key(&record).unwrap();
        assert!(admit_csharp_receipt_row(&key, &record).is_ok());
        assert!(admit_csharp_receipt_row(&[0; 32], &record).is_err());
        assert!(csharp_receipt_key(&member("../A.cs")).is_err());
        let quote_component = "\"".repeat(235);
        let long_path = format!("{}/A.cs", vec![quote_component.as_str(); 17].join("/"));
        let mut oversize = member(&long_path);
        if let CSharpReceiptRecord::Member { unit, .. } = &mut oversize {
            *unit = CSharpReceiptUnit::Project {
                path: format!("{}/A.csproj", vec![quote_component.as_str(); 17].join("/")),
            };
        }
        assert!(csharp_receipt_key(&oversize).is_err());
        let mut without_invocation = record.clone();
        if let CSharpReceiptRecord::Member {
            outcome: CSharpMemberOutcome::Published { invocation, .. },
            ..
        } = &mut without_invocation
        {
            *invocation = [0; 32];
        }
        assert!(csharp_receipt_key(&without_invocation).is_err());
        let mut encoded = Vec::new();
        CSharpReceiptRelation::encode_value(&record, &mut encoded);
        assert_eq!(
            CSharpReceiptRelation::decode_value(&encoded).unwrap(),
            record
        );
        encoded.push(b' ');
        assert!(CSharpReceiptRelation::decode_value(&encoded).is_err());
        assert!(CSharpReceiptRelation::decode_value(b"CSR0{}").is_err());
    }
}
