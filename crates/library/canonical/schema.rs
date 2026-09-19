//! Product schema markers and the canonical view relation.

use super::frontier::Frontier;
use crate::{Basis, Coverage, Row, RowId};
use backend_version::{
    DeltaId as VersionDeltaId, ObjectKey, ObjectVersion, Relation, Schema, StateRoot,
};

/// Protocol/schema version shared by view roots and transport cursors.
pub const PROTOCOL_SCHEMA: u16 = 2;

/// Canonical schema for actor logical identities.
pub struct ActorSchema;
impl Schema for ActorSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 1;
    type Value = str;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value.as_bytes());
    }
}

/// Canonical schema for package logical identities.
pub struct PackageSchema;
impl Schema for PackageSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 2;
    type Value = str;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value.as_bytes());
    }
}

/// Canonical schema for declaration logical identities.
pub struct SymbolSchema;
impl Schema for SymbolSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 3;
    type Value = str;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value.as_bytes());
    }
}

/// Canonical schema for immutable object values.
pub struct ObjectSchema;
impl Schema for ObjectSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 4;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for immutable view recipe identities.
pub struct ViewRecipeSchema;
impl Schema for ViewRecipeSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 5;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for immutable view versions.
pub struct ViewVersionSchema;
impl Schema for ViewVersionSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 11;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for durable user intent identities.
pub struct IntentSchema;
impl Schema for IntentSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 12;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for branch logical identities.
pub struct BranchSchema;
impl Schema for BranchSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 6;
    type Value = str;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value.as_bytes());
    }
}

/// Canonical schema for append-only log logical identities.
pub struct LogSchema;
impl Schema for LogSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 7;
    type Value = str;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value.as_bytes());
    }
}

/// Canonical schema for document payload values.
pub struct DocumentSchema;
impl Schema for DocumentSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 8;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for a name-query recipe/value.
pub struct NameSchema;
impl Schema for NameSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 9;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for an outline-query recipe/value.
pub struct OutlineSchema;
impl Schema for OutlineSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 10;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical ordered relation containing complete visible view rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewRelation;
impl Relation for ViewRelation {
    const DOMAIN: u8 = 0x20;
    const TYPE: u16 = 1;
    type Key = ViewEntryKey;
    type Value = ViewEntry;

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        match value {
            ViewEntryKey::Metadata => out.push(0),
            ViewEntryKey::Row(id) => {
                out.push(1);
                super::encoding::append_row_id(out, *id);
            }
        }
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        match value {
            ViewEntry::Metadata(value) => super::encoding::encode_metadata(value, out),
            ViewEntry::Row(value) => super::encoding::encode_row(value, out),
        }
    }
}

/// Type-level key namespace for the canonical view relation.
///
/// Metadata is a first-class entry rather than a digest sentinel in the row
/// namespace. This keeps the row identity boundary exact while ensuring the
/// relation root changes when frontier or coverage metadata changes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ViewEntryKey {
    /// One metadata entry for the immutable view basis/frontier/coverage.
    Metadata,
    /// One visible row keyed by its stable identity.
    Row(RowId),
}

/// Canonical value stored in the view relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewEntry {
    /// Basis/frontier/coverage commitment metadata.
    Metadata(ViewMetadata),
    /// One complete visible row.
    Row(Row),
}

/// Semantic metadata committed into every view relation root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewMetadata {
    /// Exact source basis.
    pub basis: Basis,
    /// Source branch/log/schema/sequence frontier.
    pub frontier: Frontier,
    /// Honest lane coverage reports.
    pub coverage: Box<[Coverage]>,
}

impl ViewMetadata {
    /// Creates the exact metadata value committed for one view root.
    #[must_use]
    pub fn new(basis: Basis, frontier: Frontier, coverage: &[Coverage]) -> Self {
        Self {
            basis,
            frontier,
            coverage: coverage.to_vec().into_boxed_slice(),
        }
    }
}

/// Accepted actor key.
pub type ActorKey = ObjectKey<ActorSchema>;
/// Accepted package key.
pub type PackageKey = ObjectKey<PackageSchema>;
/// Accepted declaration key.
pub type SymbolKey = ObjectKey<SymbolSchema>;
/// Accepted immutable object version.
pub type SemanticObject = ObjectVersion<ObjectSchema>;
/// Accepted immutable view recipe identity.
pub type ViewRecipeId = ObjectKey<ViewRecipeSchema>;
/// Accepted immutable view version.
pub type ViewVersion = ObjectVersion<ViewVersionSchema>;
/// Accepted durable intent/idempotency identity.
pub type IntentId = ObjectVersion<IntentSchema>;
/// Accepted branch key.
pub type BranchKey = ObjectKey<BranchSchema>;
/// Accepted log key.
pub type LogKey = ObjectKey<LogSchema>;
/// Accepted document version.
pub type DocumentVersion = ObjectVersion<DocumentSchema>;
/// Accepted name recipe version.
pub type NameVersion = ObjectVersion<NameSchema>;
/// Accepted outline recipe version.
pub type OutlineVersion = ObjectVersion<OutlineSchema>;
/// Accepted visible view relation root.
pub type ViewStateRoot = StateRoot<ViewRelation>;
/// Accepted visible view relation transition identity.
pub type ViewDeltaId = VersionDeltaId<ViewRelation>;
