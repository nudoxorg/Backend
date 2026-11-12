//! Bounded persisted claims for native source bytes without structural inventory.
//!
//! A decoded row is not proof of file ownership or byte identity. Admission must
//! join the selected project and native capture/image and verify actual confined
//! host bytes in both content domains. Missing rows mean unknown inventory.
use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_semantic::vocabulary::LanguageProfile;
use backend_store::TypedObject;
use backend_version::{
    CanonicalRelation, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity, StateRoot,
};
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"NSE1";
const ROOT_KEY: [u8; 32] = [0x45; 32];
/// Maximum path and captured input sizes, matching native source admission.
pub const MAX_NATIVE_EVIDENCE_PATH: usize = 4096;
pub const MAX_NATIVE_EVIDENCE_SOURCE: u32 = 64 * 1024 * 1024;
/// Limits include canonical keys, values, and per-row framing.
pub const MAX_NATIVE_EVIDENCE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_NATIVE_EVIDENCE_ROWS: usize = 262_144;
pub const MAX_NATIVE_EVIDENCE_PROJECT_ROWS: usize = 100_000;
/// Maximum canonical row value; framing and key are charged separately.
pub const MAX_NATIVE_EVIDENCE_VALUE: usize =
    4 + 32 + 2 + 2 + MAX_NATIVE_EVIDENCE_PATH + 32 + 32 + 4 + 1;

/// Independently selected native-source claim relation.
#[derive(Debug)]
pub struct NativeSourceEvidenceRelation;
/// Typed pointer to the selected evidence root.
pub struct NativeSourceEvidenceRootSchema;
impl Schema for NativeSourceEvidenceRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 15;
    const VERSION: u8 = 1;
    type Value = [u8; 32];
    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Untrusted persisted claims, not a verified source capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeSourceEvidence {
    pub project: [u8; 32],
    pub profile: LanguageProfile,
    pub path: String,
    /// Raw expected input digest; it cannot be converted into a typed identity.
    pub input_digest: [u8; 32],
    /// Raw expected source fact digest; verification mints the typed identity from bytes.
    pub source_digest: [u8; 32],
    pub byte_length: u32,
}
impl NativeSourceEvidence {
    /// No declaration inventory completeness follows from this row.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.byte_length > MAX_NATIVE_EVIDENCE_SOURCE {
            return Err("invalid native source evidence length");
        }
        native_source_evidence_key(self.project, self.profile, &self.path).map(|_| ())
    }
    /// Checks actual bytes against this claim and an independently admitted image identity.
    /// This does not establish workspace selection, ownership, or freshness.
    pub fn verify_host<'a>(
        &self,
        bytes: &'a [u8],
        image_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    ) -> Result<backend_compile::VerifiedSourceText<'a>, &'static str> {
        self.validate()?;
        if bytes.len() != self.byte_length as usize {
            return Err("native source length differs");
        }
        if image_source.as_ref() != self.source_digest.as_slice() {
            return Err("native image source differs from evidence");
        }
        backend_compile::VerifiedSourceText::admit_stored(bytes, self.input_digest, image_source)
            .map_err(|_| "native source bytes do not match evidence")
    }
    /// Key binds the complete canonical owner, profile and path.
    pub fn key(&self) -> Result<[u8; 32], &'static str> {
        if self.byte_length > MAX_NATIVE_EVIDENCE_SOURCE {
            return Err("invalid native source evidence length");
        }
        native_source_evidence_key(self.project, self.profile, &self.path)
    }
}
/// Derives a lookup key without constructing or trusting a source claim.
pub fn native_source_evidence_key(
    project: [u8; 32],
    profile: LanguageProfile,
    path: &str,
) -> Result<[u8; 32], &'static str> {
    if project == [0; 32]
        || path.is_empty()
        || path.len() > MAX_NATIVE_EVIDENCE_PATH
        || path.starts_with('/')
        || path.contains('\\')
        || path.bytes().any(|b| b.is_ascii_control())
        || (path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic())
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("invalid native source evidence owner");
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.native-source-evidence.v1\0");
    hash.update(&project);
    hash.update(&<[u8; 2]>::from(profile));
    hash.update(&(path.len() as u32).to_be_bytes());
    hash.update(path.as_bytes());
    Ok(*hash.finalize().as_bytes())
}
impl Relation for NativeSourceEvidenceRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 14;
    const VERSION: u8 = 1;
    type Key = [u8; 32];
    type Value = NativeSourceEvidence;
    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&value.project);
        output.extend_from_slice(&<[u8; 2]>::from(value.profile));
        output.extend_from_slice(&(value.path.len() as u16).to_be_bytes());
        output.extend_from_slice(value.path.as_bytes());
        output.extend_from_slice(&value.input_digest);
        output.extend_from_slice(&value.source_digest);
        output.extend_from_slice(&value.byte_length.to_be_bytes());
        output.push(0); // InventoryUnknown; no complete-inventory claim.
    }
}
impl CanonicalRelation for NativeSourceEvidenceRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes.try_into().map_err(|_| RelationDecodeError::Malformed)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        fn parse(bytes: &[u8]) -> Option<NativeSourceEvidence> {
            if bytes.len() > MAX_NATIVE_EVIDENCE_VALUE || bytes.get(..4)? != MAGIC {
                return None;
            }
            let project = bytes.get(4..36)?.try_into().ok()?;
            let profile =
                LanguageProfile::try_from(<[u8; 2]>::try_from(bytes.get(36..38)?).ok()?).ok()?;
            let len = u16::from_be_bytes(bytes.get(38..40)?.try_into().ok()?) as usize;
            let end = 40usize.checked_add(len)?;
            let path = std::str::from_utf8(bytes.get(40..end)?).ok()?.to_owned();
            let rest = bytes.get(end..)?;
            if rest.len() != 69 || rest[68] != 0 {
                return None;
            }
            let record = NativeSourceEvidence {
                project,
                profile,
                path,
                input_digest: rest.get(..32)?.try_into().ok()?,
                source_digest: rest.get(32..64)?.try_into().ok()?,
                byte_length: u32::from_be_bytes(rest.get(64..68)?.try_into().ok()?),
            };
            record.validate().ok()?;
            Some(record)
        }
        parse(bytes).ok_or(RelationDecodeError::Malformed)
    }
}
/// Validates canonical ownership, ordering, and exact encoded work before staging.
pub fn validate_native_source_evidence_rows(
    rows: &[([u8; 32], NativeSourceEvidence)],
) -> Result<(), &'static str> {
    if rows.len() > MAX_NATIVE_EVIDENCE_ROWS {
        return Err("native source evidence row limit");
    }
    let mut bytes = 0usize;
    let mut previous = None;
    let mut projects = BTreeMap::<[u8; 32], usize>::new();
    for (key, row) in rows {
        if previous.is_some_and(|prior| prior >= *key) || row.key()? != *key {
            return Err("noncanonical native source evidence key");
        }
        previous = Some(*key);
        let count = projects.entry(row.project).or_default();
        *count = count
            .checked_add(1)
            .ok_or("native source evidence count overflow")?;
        if *count > MAX_NATIVE_EVIDENCE_PROJECT_ROWS {
            return Err("native source evidence project limit");
        }
        let mut encoded = Vec::new();
        NativeSourceEvidenceRelation::encode_value(row, &mut encoded);
        // One 32-byte key plus two u32 lengths frames each exact record.
        bytes = bytes
            .checked_add(32 + 8)
            .and_then(|n| n.checked_add(encoded.len()))
            .ok_or("native source evidence byte overflow")?;
        if bytes > MAX_NATIVE_EVIDENCE_BYTES {
            return Err("native source evidence byte limit");
        }
    }
    Ok(())
}
/// Canonical pointer key for selected evidence.
pub fn native_source_evidence_root_pointer_key() -> [u8; 32] {
    *ObjectKey::<NativeSourceEvidenceRootSchema>::from_value(&ROOT_KEY).as_bytes()
}
/// Makes a pointer; authority is established by the selected workspace closure.
pub fn native_source_evidence_root_object(
    root: StateRoot<NativeSourceEvidenceRelation>,
) -> TypedObject {
    let key = ObjectKey::<NativeSourceEvidenceRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}
/// Opens selected evidence; absent root remains unknown.
pub fn native_source_evidence_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceRelationHandle<NativeSourceEvidenceRelation>>, WorkspaceRelationError> {
    let key = ObjectKey::<NativeSourceEvidenceRootSchema>::from_value(&ROOT_KEY);
    snapshot.auxiliary_relation::<NativeSourceEvidenceRelation>(
        SchemaIdentity::new(
            NativeSourceEvidenceRootSchema::DOMAIN,
            NativeSourceEvidenceRootSchema::TYPE,
            NativeSourceEvidenceRootSchema::VERSION,
        ),
        *key.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{GoVersion, LanguageProfile};

    fn row() -> NativeSourceEvidence {
        NativeSourceEvidence {
            project: [7; 32],
            profile: LanguageProfile::Go(GoVersion::Go125),
            path: "pkg/π.go".into(),
            input_digest: [8; 32],
            source_digest: [9; 32],
            byte_length: 42,
        }
    }

    #[test]
    fn selected_pointer_preflight_charges_before_root_and_child_decode() {
        use backend_store::{FileStore, RelationAdmissionRegistry, StoreError};
        use backend_version::{CoverageWitness, RelationState, ScopeRoot, UntrustedCoverageScope};
        use std::sync::Arc;
        let directory=tempfile::tempdir().unwrap();
        let registry=RelationAdmissionRegistry::new().with_relation::<NativeSourceEvidenceRelation>().unwrap();
        let store=FileStore::open_with_registry(directory.path(),1024*1024,registry).unwrap();
        let mut rows=(0..200).map(|i| {
            let mut r=row();r.path=format!("pkg/{i:03}.go");r.byte_length=2048;
            r.line_starts=Some((0..1024).map(|n|n*2).collect());
            (r.key().unwrap(),r)
        }).collect::<Vec<_>>();
        rows.sort_unstable_by_key(|r|r.0);
        let coverage=CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(ScopeRoot::from_u64(1)));
        let state=RelationState::<NativeSourceEvidenceRelation>::from_entries(rows.clone(),coverage).unwrap();
        store.write_relation_state(&state).unwrap();
        let handle=crate::workspace::WorkspacePreflightRelationHandle::<NativeSourceEvidenceRelation>::new(Arc::new(store),*state.root().as_bytes()).unwrap();
        let calls=std::cell::Cell::new(0usize);
        let mut gate=|node: &backend_version::CanonicalNodeView<'_>, budget: &mut usize| {
            calls.set(calls.get()+1);
            if node.is_leaf() { for field in node.leaf_field_iter().map_err(|_|StoreError::Corrupt)? { field.map_err(|_|StoreError::Corrupt)?; } }
            *budget=budget.checked_sub(128*1024).ok_or(StoreError::Bounds)?;
            Ok(())
        };
        let mut short=0;
        assert!(handle.lookup(&rows[0].0,32,&mut short,&mut gate).is_err());
        assert_eq!(calls.get(),0);
        let mut budget=16*1024*1024;
        let got=handle.lookup(&rows[0].0,32,&mut budget,&mut gate).unwrap().unwrap();
        assert_eq!(got,rows[0].1);
        assert!(calls.get()>=2, "root and child must each be preflighted");
        assert!(budget<16*1024*1024);
        let mut visited = Vec::new();
        let mut cursor = None;
        let mut visitor = |key: &[u8; 32], row: NativeSourceEvidence, budget: &mut usize| {
            let retained = row.checked_heap_bytes().and_then(|n| n.checked_add(std::mem::size_of::<NativeSourceEvidence>() + 128)).ok_or(StoreError::Bounds)?;
            *budget = budget.checked_sub(retained).ok_or(StoreError::Bounds)?;
            visited.push((*key, row));
            Ok(())
        };
        loop {
            cursor = handle.visit(cursor.as_ref(), 64, 32, &mut budget, &mut gate, &mut visitor).unwrap();
            if cursor.is_none() { break; }
        }
        drop(visitor);
        assert_eq!(visited, rows);
    }

    #[test]
    fn claims_roundtrip_and_reject_noncanonical_input() {
        let row = row();
        let mut bytes = Vec::new();
        NativeSourceEvidenceRelation::encode_value(&row, &mut bytes);
        assert_eq!(
            NativeSourceEvidenceRelation::decode_value(&bytes).unwrap(),
            row
        );
        let last = bytes.len() - 1;
        bytes[last] = 1; // Unknown inventory is the only represented status.
        assert!(NativeSourceEvidenceRelation::decode_value(&bytes).is_err());
        bytes[last] = 0;
        bytes.push(0);
        assert!(NativeSourceEvidenceRelation::decode_value(&bytes).is_err());
        for path in ["", "../x.go", "/x.go", "a//x.go", "C:/x.go", "a\\x.go"] {
            let mut invalid = row.clone();
            invalid.path = path.into();
            assert!(invalid.key().is_err());
        }
        let mut too_large = row;
        too_large.byte_length = MAX_NATIVE_EVIDENCE_SOURCE + 1;
        assert!(too_large.key().is_err());
    }

    #[test]
    fn host_verification_checks_both_domains_and_length() {
        let source = "π emoji 😀".as_bytes();
        let image =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                source,
            );
        let mut claim = row();
        claim.byte_length = source.len() as u32;
        claim.input_digest =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(source).to_bytes();
        claim.source_digest.copy_from_slice(image.as_ref());
        assert!(claim.verify_host(source, image).is_ok());
        assert!(claim.verify_host("λ emoji 😀".as_bytes(), image).is_err());
        let mut wrong = claim.clone();
        wrong.input_digest[0] ^= 1;
        assert!(wrong.verify_host(source, image).is_err());
        wrong = claim.clone();
        wrong.source_digest[0] ^= 1;
        assert!(wrong.verify_host(source, image).is_err());
        wrong = claim;
        wrong.byte_length -= 1;
        assert!(wrong.verify_host(source, image).is_err());
    }

    #[test]
    fn aggregate_and_per_project_budgets_are_enforced() {
        let mut rows = Vec::with_capacity(MAX_NATIVE_EVIDENCE_PROJECT_ROWS + 1);
        for i in 0..=MAX_NATIVE_EVIDENCE_PROJECT_ROWS {
            let mut item = row();
            item.path = format!("p/{i}.go");
            rows.push((item.key().unwrap(), item));
        }
        rows.sort_unstable_by_key(|entry| entry.0);
        assert_eq!(
            validate_native_source_evidence_rows(&rows),
            Err("native source evidence project limit")
        );
        drop(rows);
        let mut rows = Vec::new();
        for i in 0..17_000 {
            let mut item = row();
            item.path = format!("{i:05}{}", "x".repeat(MAX_NATIVE_EVIDENCE_PATH - 5));
            rows.push((item.key().unwrap(), item));
        }
        rows.sort_unstable_by_key(|entry| entry.0);
        assert_eq!(
            validate_native_source_evidence_rows(&rows),
            Err("native source evidence byte limit")
        );
    }

    #[test]
    fn staging_requires_matching_unique_ordered_keys() {
        let first = row();
        let key = first.key().unwrap();
        assert_eq!(
            native_source_evidence_key(first.project, first.profile, &first.path),
            Ok(key)
        );
        assert!(validate_native_source_evidence_rows(&[(key, first.clone())]).is_ok());
        assert!(validate_native_source_evidence_rows(&[([0; 32], first.clone())]).is_err());
        assert!(
            validate_native_source_evidence_rows(&[(key, first.clone()), (key, first)]).is_err()
        );
    }
}
