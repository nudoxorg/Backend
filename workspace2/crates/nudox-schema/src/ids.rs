use core::fmt;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

/// Compact closed Wave 1 schema registry with explicit stable wire tags.
#[repr(u32)]
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Immutable,
    IntoBytes,
    KnownLayout,
    Ord,
    PartialEq,
    PartialOrd,
    TryFromBytes,
)]
pub enum SchemaId {
    /// Canonical immutable object bytes described by `nudox-object`.
    Object = 1_u32.to_be(),
    /// Canonical bounded batch frame body described by this crate.
    Frame = 0x0001_0001_u32.to_be(),
    /// Canonical compact semantic IR fragment bytes.
    IrFragment = 0x0002_0001_u32.to_be(),
    /// Canonical compiler publication manifest bytes.
    CompilationManifest = 0x0002_0002_u32.to_be(),
}

impl From<SchemaId> for u32 {
    fn from(schema: SchemaId) -> Self {
        match schema {
            SchemaId::Object => 1,
            SchemaId::Frame => 0x0001_0001,
            SchemaId::IrFragment => 0x0002_0001,
            SchemaId::CompilationManifest => 0x0002_0002,
        }
    }
}

impl TryFrom<u32> for SchemaId {
    type Error = UnknownSchemaId;

    fn try_from(wire: u32) -> Result<Self, Self::Error> {
        match wire {
            1 => Ok(Self::Object),
            0x0001_0001 => Ok(Self::Frame),
            0x0002_0001 => Ok(Self::IrFragment),
            0x0002_0002 => Ok(Self::CompilationManifest),
            other => Err(UnknownSchemaId(other)),
        }
    }
}

impl fmt::Display for SchemaId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Object => formatter.write_str("schema:object"),
            Self::Frame => formatter.write_str("schema:frame"),
            Self::IrFragment => formatter.write_str("schema:ir-fragment"),
            Self::CompilationManifest => formatter.write_str("schema:compilation-manifest"),
        }
    }
}

/// Rejected raw schema tag retained for precise compatibility errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("unknown schema tag {0:#010x}")]
pub struct UnknownSchemaId(pub u32);

/// Compact closed Wave 1 operation registry.
#[repr(u32)]
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Immutable,
    IntoBytes,
    KnownLayout,
    Ord,
    PartialEq,
    PartialOrd,
    TryFromBytes,
)]
pub enum OperationId {
    /// Static pinned-object operation defined by `nudox-operation`.
    PinnedObject = 1_u32.to_be(),
}

impl From<OperationId> for u32 {
    fn from(operation: OperationId) -> Self {
        match operation {
            OperationId::PinnedObject => 1,
        }
    }
}

impl TryFrom<u32> for OperationId {
    type Error = UnknownOperationId;

    fn try_from(wire: u32) -> Result<Self, Self::Error> {
        match wire {
            1 => Ok(Self::PinnedObject),
            other => Err(UnknownOperationId(other)),
        }
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PinnedObject => formatter.write_str("operation:pinned-object"),
        }
    }
}

/// Rejected raw operation tag retained for precise dispatch errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("unknown operation tag {0:#010x}")]
pub struct UnknownOperationId(pub u32);

#[cfg(test)]
mod tests {
    use core::mem::{align_of, size_of};
    use zerocopy::IntoBytes;

    use super::{OperationId, SchemaId, UnknownOperationId, UnknownSchemaId};

    #[test]
    fn closed_tags_are_unique_compact_and_round_trip() {
        assert_ne!(u32::from(SchemaId::Object), u32::from(SchemaId::Frame));
        assert_ne!(u32::from(SchemaId::Frame), u32::from(SchemaId::IrFragment));
        assert_ne!(
            u32::from(SchemaId::IrFragment),
            u32::from(SchemaId::CompilationManifest)
        );
        assert_eq!(
            SchemaId::try_from(u32::from(SchemaId::Frame)),
            Ok(SchemaId::Frame)
        );
        assert_eq!(
            SchemaId::try_from(u32::from(SchemaId::IrFragment)),
            Ok(SchemaId::IrFragment)
        );
        assert_eq!(
            SchemaId::try_from(u32::from(SchemaId::CompilationManifest)),
            Ok(SchemaId::CompilationManifest)
        );
        assert_eq!(OperationId::try_from(1), Ok(OperationId::PinnedObject));
        assert_eq!(SchemaId::try_from(99), Err(UnknownSchemaId(99)));
        assert_eq!(OperationId::try_from(99), Err(UnknownOperationId(99)));
    }

    #[test]
    fn closed_tags_are_native_types_with_canonical_big_endian_memory() {
        assert_eq!((size_of::<SchemaId>(), align_of::<SchemaId>()), (4, 4));
        assert_eq!(
            (size_of::<OperationId>(), align_of::<OperationId>()),
            (4, 4)
        );
        assert_eq!(SchemaId::Object.as_bytes(), 1_u32.to_be_bytes());
        assert_eq!(SchemaId::Frame.as_bytes(), 0x0001_0001_u32.to_be_bytes());
        assert_eq!(
            SchemaId::IrFragment.as_bytes(),
            0x0002_0001_u32.to_be_bytes()
        );
        assert_eq!(
            SchemaId::CompilationManifest.as_bytes(),
            0x0002_0002_u32.to_be_bytes()
        );
        assert_eq!(OperationId::PinnedObject.as_bytes(), 1_u32.to_be_bytes());
    }
}
