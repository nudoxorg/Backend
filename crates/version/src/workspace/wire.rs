/// Maximum accepted size of one version-layer wire value.
pub const MAX_WORKSPACE_WIRE_BYTES: usize = 64 * 1024;

/// Maximum number of repeated entries in one bounded wire value.
pub const MAX_WORKSPACE_WIRE_ITEMS: usize = 4096;

/// Current version of the workspace-layer wire envelopes.
///
/// Version 2 adds producer/session/evidence identity to complete coverage
/// records. Keeping one envelope version across manifests, transitions,
/// deltas, provenance, and commits ensures an older decoder rejects the new
/// manifest grammar instead of misparsing its shifted fields.
pub const WORKSPACE_WIRE_VERSION: u8 = 2;

const MANIFEST_WIRE_MAGIC: [u8; 4] = *b"WMF2";
const TRANSITION_WIRE_MAGIC: [u8; 4] = *b"WTR2";
const DELTA_WIRE_MAGIC: [u8; 4] = *b"WDL2";
const PROVENANCE_WIRE_MAGIC: [u8; 4] = *b"WPR2";
const COMMIT_WIRE_MAGIC: [u8; 4] = *b"WCM2";
const MAX_WIRE_DETAIL_BYTES: usize = 16 * 1024;

/// Failure while parsing a bounded workspace-layer wire envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceDecodeError {
    /// The envelope exceeded [`MAX_WORKSPACE_WIRE_BYTES`].
    TooLarge,
    /// The envelope ended before a complete field was available.
    Truncated,
    /// The envelope's object-kind marker was not recognized.
    InvalidMagic,
    /// The envelope used a wire version this crate does not understand.
    UnsupportedVersion,
    /// A tag or enum discriminant was outside the canonical grammar.
    InvalidTag,
    /// A length field was not representable or exceeded its field budget.
    InvalidLength,
    /// A bounded repeated field exceeded [`MAX_WORKSPACE_WIRE_ITEMS`].
    TooManyItems,
    /// Bytes remained after the complete canonical envelope.
    TrailingBytes,
    /// A semantic workspace invariant failed after structural decoding.
    Semantic(WorkspaceError),
    /// Parent IDs were not strictly ordered in a wire commit.
    UnorderedParents,
    /// A wire commit repeated one parent ID.
    DuplicateParent,
}

impl fmt::Display for WorkspaceDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid workspace wire value: {self:?}")
    }
}

impl std::error::Error for WorkspaceDecodeError {}

struct WireReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> WireReader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, WorkspaceDecodeError> {
        Self::new_with_limit(bytes, MAX_WORKSPACE_WIRE_BYTES)
    }

    fn new_with_limit(bytes: &'a [u8], max_bytes: usize) -> Result<Self, WorkspaceDecodeError> {
        if bytes.len() > max_bytes {
            return Err(WorkspaceDecodeError::TooLarge);
        }
        Ok(Self { bytes, offset: 0 })
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], WorkspaceDecodeError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(WorkspaceDecodeError::InvalidLength)?;
        if end > self.bytes.len() {
            return Err(WorkspaceDecodeError::Truncated);
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, WorkspaceDecodeError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(WorkspaceDecodeError::Truncated)
    }

    fn u16(&mut self) -> Result<u16, WorkspaceDecodeError> {
        let value = self.take(2)?;
        let bytes: [u8; 2] = value
            .try_into()
            .map_err(|_| WorkspaceDecodeError::Truncated)?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, WorkspaceDecodeError> {
        let value = self.take(4)?;
        let bytes: [u8; 4] = value
            .try_into()
            .map_err(|_| WorkspaceDecodeError::Truncated)?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, WorkspaceDecodeError> {
        let value = self.take(8)?;
        let bytes: [u8; 8] = value
            .try_into()
            .map_err(|_| WorkspaceDecodeError::Truncated)?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn digest(&mut self) -> Result<[u8; ID_BYTES], WorkspaceDecodeError> {
        let value = self.take(ID_BYTES)?;
        value
            .try_into()
            .map_err(|_| WorkspaceDecodeError::Truncated)
    }

    fn field(&mut self) -> Result<&'a [u8], WorkspaceDecodeError> {
        let length =
            usize::try_from(self.u64()?).map_err(|_| WorkspaceDecodeError::InvalidLength)?;
        self.take(length)
    }

    fn count(&mut self) -> Result<usize, WorkspaceDecodeError> {
        let count =
            usize::try_from(self.u32()?).map_err(|_| WorkspaceDecodeError::InvalidLength)?;
        if count > MAX_WORKSPACE_WIRE_ITEMS {
            return Err(WorkspaceDecodeError::TooManyItems);
        }
        Ok(count)
    }

    fn magic(&mut self, expected: [u8; 4]) -> Result<(), WorkspaceDecodeError> {
        if self.take(4)? != expected {
            return Err(WorkspaceDecodeError::InvalidMagic);
        }
        if self.byte()? != WORKSPACE_WIRE_VERSION {
            return Err(WorkspaceDecodeError::UnsupportedVersion);
        }
        Ok(())
    }

    fn finish(self) -> Result<(), WorkspaceDecodeError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(WorkspaceDecodeError::TrailingBytes)
        }
    }
}

fn push_count(out: &mut Vec<u8>, count: usize) {
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_be_bytes());
}

fn push_schema(out: &mut Vec<u8>, schema: SchemaIdentity) {
    out.push(schema.domain());
    out.extend_from_slice(&schema.ty().to_be_bytes());
    out.push(schema.version());
}

fn read_schema(reader: &mut WireReader<'_>) -> Result<SchemaIdentity, WorkspaceDecodeError> {
    Ok(SchemaIdentity::new(
        reader.byte()?,
        reader.u16()?,
        reader.byte()?,
    ))
}

fn push_digest(out: &mut Vec<u8>, digest: [u8; ID_BYTES]) {
    out.extend_from_slice(&digest);
}

fn append_wire_field(out: &mut Vec<u8>, bytes: &[u8]) {
    append_field(out, |inner| inner.extend_from_slice(bytes));
}
