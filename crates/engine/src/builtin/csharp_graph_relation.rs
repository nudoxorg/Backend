//! Persisted C# graph claims. Rows select no compiler or source authority.
use crate::application::{decode_durable_csharp_graph_relation, encode_durable_csharp_graph_relation, DurableCSharpGraphRelation};
use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_store::TypedObject;
use backend_version::{CanonicalRelation, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity, StateRoot};

const ROOT_KEY: [u8; 32] = [0x67; 32];
const MAGIC: &[u8; 4] = b"CGR1";
const MAX_ROW_BYTES: usize = 2 * 1024 * 1024 + 36;

/// A durable graph is a claim, never an owner-selected witness.
pub struct CSharpGraphRelation;
/// Closure pointer selecting the graph claim relation.
pub struct CSharpGraphRootSchema;
impl Schema for CSharpGraphRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 25;
    const VERSION: u8 = 1;
    type Value = [u8; 32];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) { out.extend_from_slice(value); }
}

/// Package identity is included in the row and checked against its key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CSharpGraphRecord {
    pub project: [u8; 32],
    pub claim: DurableCSharpGraphRelation,
}

/// Derive the stable row key for one package and project root.
pub fn csharp_graph_record_key(record: &CSharpGraphRecord) -> Result<[u8; 32], &'static str> {
    if record.project == [0; 32] { return Err("invalid C# graph project"); }
    let encoded = encode_durable_csharp_graph_relation(&record.claim).ok_or("invalid C# graph claim")?;
    if decode_durable_csharp_graph_relation(&encoded).as_ref() != Some(&record.claim) { return Err("invalid C# graph claim"); }
    let graph = crate::application::encode_csharp_graph_evidence(&record.claim.graph).ok_or("invalid C# graph")?;
    if crate::application::decode_csharp_graph_evidence(&graph).as_ref() != Some(&record.claim.graph) { return Err("invalid C# graph"); }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.csharp-graph.key.v1\0");
    hash.update(&record.project);
    let path = record.claim.graph.root_project.as_bytes();
    hash.update(&(path.len() as u64).to_be_bytes());
    hash.update(path);
    Ok(*hash.finalize().as_bytes())
}
impl Relation for CSharpGraphRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 24;
    const VERSION: u8 = 1;
    type Key = [u8; 32];
    type Value = CSharpGraphRecord;
    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) { out.extend_from_slice(key); }
    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&value.project);
        out.extend_from_slice(&encode_durable_csharp_graph_relation(&value.claim).expect("serializable C# graph claim"));
    }
}
impl CanonicalRelation for CSharpGraphRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        let key: [u8; 32] = bytes.try_into().map_err(|_| RelationDecodeError::Malformed)?;
        if key == [0; 32] { return Err(RelationDecodeError::Malformed); }
        Ok(key)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        if bytes.len() <= 36 || bytes.len() > MAX_ROW_BYTES || !bytes.starts_with(MAGIC) { return Err(RelationDecodeError::Malformed); }
        let project = bytes[4..36].try_into().map_err(|_| RelationDecodeError::Malformed)?;
        let claim = decode_durable_csharp_graph_relation(&bytes[36..]).ok_or(RelationDecodeError::Malformed)?;
        let value = CSharpGraphRecord { project, claim };
        csharp_graph_record_key(&value).map_err(|_| RelationDecodeError::Malformed)?;
        let mut canonical = Vec::new();
        Self::encode_value(&value, &mut canonical);
        if canonical != bytes { return Err(RelationDecodeError::Malformed); }
        Ok(value)
    }
}
/// Verify the key and record are paired. This does not grant publication authority.
pub fn admit_csharp_graph_row(key: &[u8; 32], row: &CSharpGraphRecord) -> Result<(), &'static str> {
    if csharp_graph_record_key(row)? != *key { return Err("C# graph key mismatch"); }
    Ok(())
}
pub fn csharp_graph_root_pointer_key() -> [u8; 32] {
    *ObjectKey::<CSharpGraphRootSchema>::from_value(&ROOT_KEY).as_bytes()
}
pub fn csharp_graph_root_object(root: StateRoot<CSharpGraphRelation>) -> TypedObject {
    TypedObject::from_value(&ObjectKey::<CSharpGraphRootSchema>::from_value(&ROOT_KEY), root.as_bytes())
}
pub fn csharp_graph_relation(snapshot: &WorkspaceSnapshot) -> Result<Option<WorkspaceRelationHandle<CSharpGraphRelation>>, WorkspaceRelationError> {
    snapshot.auxiliary_relation::<CSharpGraphRelation>(SchemaIdentity::new(CSharpGraphRootSchema::DOMAIN, CSharpGraphRootSchema::TYPE, CSharpGraphRootSchema::VERSION), csharp_graph_root_pointer_key())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{CSharpGraphEvidence, CSharpGraphEvidenceNode};

    fn row() -> CSharpGraphRecord {
        CSharpGraphRecord { project: [1; 32], claim: DurableCSharpGraphRelation {
            format: 1, operation: [2; 32], attempt: [3; 32], owner_epoch: 1, snapshot: [4; 32],
            graph: CSharpGraphEvidence { root_project: "App/App.csproj".into(), config_digest: [5; 32], source_digest: [6; 32], reference_digest: [7; 32],
                nodes: vec![CSharpGraphEvidenceNode { project_path: "App/App.csproj".into(), project_references: vec![], config_digest: [8; 32], source_digest: [9; 32] }] },
        }}
    }
    #[test]
    fn canonical_claim_and_exact_key_are_required() {
        let row = row();
        let key = csharp_graph_record_key(&row).unwrap();
        assert!(admit_csharp_graph_row(&key, &row).is_ok());
        assert!(admit_csharp_graph_row(&[9; 32], &row).is_err());
        let mut bytes = Vec::new();
        CSharpGraphRelation::encode_value(&row, &mut bytes);
        assert_eq!(CSharpGraphRelation::decode_value(&bytes).unwrap(), row);
        bytes.push(b' ');
        assert!(CSharpGraphRelation::decode_value(&bytes).is_err());
        assert!(CSharpGraphRelation::decode_value(&bytes[..35]).is_err());
        let mut invalid = row.clone();
        invalid.claim.snapshot = [0; 32];
        assert!(csharp_graph_record_key(&invalid).is_err());
        invalid = row.clone();
        invalid.claim.graph.nodes[0].project_references.push("missing.csproj".into());
        assert!(csharp_graph_record_key(&invalid).is_err());
    }
}
