//! Paged selection claims for per-project SQL receipts. Decoding an index row
//! does not authenticate its referenced object, its MAC, or its custody.
use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_store::TypedObject;
use backend_version::{CanonicalRelation, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity, StateRoot};

const MAGIC: &[u8; 4] = b"SRI1";
const ROOT_KEY: [u8; 32] = [0x72; 32];

/// Paged project-key to immutable receipt selection, not receipt authority.
pub struct SqlReceiptIndexRelation;
/// Selected root pointer for the SQL receipt index.
pub struct SqlReceiptIndexRootSchema;
/// Immutable checked-result receipt object; contents still require MAC checks.
pub struct SqlCheckedReceiptSchema;
impl Schema for SqlReceiptIndexRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 23;
    const VERSION: u8 = 1;
    type Value = [u8; 32];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) { out.extend_from_slice(value); }
}
impl Schema for SqlCheckedReceiptSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc00d;
    const VERSION: u8 = 1;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) { out.extend_from_slice(value); }
}
/// Raw immutable object identity/version claims. Reopen and verify the exact
/// typed object and selected closure before treating either as evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlReceiptIndexRow {
    pub object_id: [u8; 32],
    pub object_version: [u8; 32],
}
impl Relation for SqlReceiptIndexRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 22;
    const VERSION: u8 = 1;
    type Key = [u8; 32];
    type Value = SqlReceiptIndexRow;
    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) { out.extend_from_slice(value); }
    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&value.object_id);
        out.extend_from_slice(&value.object_version);
    }
}
impl CanonicalRelation for SqlReceiptIndexRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        let key: [u8; 32] = bytes.try_into().map_err(|_| RelationDecodeError::Malformed)?;
        if key == [0; 32] { return Err(RelationDecodeError::Malformed); }
        Ok(key)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        if bytes.len() != 68 || !bytes.starts_with(MAGIC) { return Err(RelationDecodeError::Malformed); }
        let object_id = bytes[4..36].try_into().map_err(|_| RelationDecodeError::Malformed)?;
        let object_version = bytes[36..68].try_into().map_err(|_| RelationDecodeError::Malformed)?;
        if object_id == [0; 32] || object_version == [0; 32] { return Err(RelationDecodeError::Malformed); }
        Ok(SqlReceiptIndexRow { object_id, object_version })
    }
}
/// Stable key of the selected index root pointer.
pub fn sql_receipt_index_root_pointer_key() -> [u8; 32] {
    *ObjectKey::<SqlReceiptIndexRootSchema>::from_value(&ROOT_KEY).as_bytes()
}
/// Construct a typed pointer to an independently admitted index root.
pub fn sql_receipt_index_root_object(root: StateRoot<SqlReceiptIndexRelation>) -> TypedObject {
    let key = ObjectKey::<SqlReceiptIndexRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}
/// Open the selected index. An absent index is unknown, never authorization.
pub fn sql_receipt_index_relation(snapshot: &WorkspaceSnapshot) -> Result<Option<WorkspaceRelationHandle<SqlReceiptIndexRelation>>, WorkspaceRelationError> {
    snapshot.auxiliary_relation::<SqlReceiptIndexRelation>(
        SchemaIdentity::new(SqlReceiptIndexRootSchema::DOMAIN, SqlReceiptIndexRootSchema::TYPE, SqlReceiptIndexRootSchema::VERSION),
        sql_receipt_index_root_pointer_key(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_index_requires_exact_bounded_nonzero_claims() {
        let row = SqlReceiptIndexRow { object_id: [1; 32], object_version: [2; 32] };
        let mut bytes = Vec::new();
        SqlReceiptIndexRelation::encode_value(&row, &mut bytes);
        assert_eq!(SqlReceiptIndexRelation::decode_value(&bytes), Ok(row));
        bytes.push(0);
        assert!(SqlReceiptIndexRelation::decode_value(&bytes).is_err());
        assert!(SqlReceiptIndexRelation::decode_key(&[0; 32]).is_err());
        assert!(SqlReceiptIndexRelation::decode_value(&[0; 68]).is_err());
    }
}
