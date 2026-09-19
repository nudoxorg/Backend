//! Bounded reader/writer and shared identity primitives.

use backend_version::IdContext;

use crate::{
    AuthorityClaim, AuthorityEpoch, ByteRange, ReplicationError, SparseCoverage, TransportLimits,
    VersionRange, WireAuthority, WireAuthorityPolicy, WireIdentity, WorkspaceRootClaim,
};

pub(super) struct Writer {
    bytes: Vec<u8>,
    max: usize,
}
impl Writer {
    pub(super) fn new(max: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max,
        }
    }
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<(), ReplicationError> {
        let next = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(ReplicationError::Overflow)?;
        if next > self.max {
            return Err(ReplicationError::MessageTooLarge);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    pub(super) fn u8(&mut self, value: u8) -> Result<(), ReplicationError> {
        self.push(&[value])
    }
    pub(super) fn u16(&mut self, value: u16) -> Result<(), ReplicationError> {
        self.push(&value.to_be_bytes())
    }
    pub(super) fn u32(&mut self, value: u32) -> Result<(), ReplicationError> {
        self.push(&value.to_be_bytes())
    }
    pub(super) fn u64(&mut self, value: u64) -> Result<(), ReplicationError> {
        self.push(&value.to_be_bytes())
    }
    pub(super) fn fixed(&mut self, value: &[u8; 32]) -> Result<(), ReplicationError> {
        self.push(value)
    }
    pub(super) fn bytes(&mut self, value: &[u8]) -> Result<(), ReplicationError> {
        let len = u32::try_from(value.len()).map_err(|_| ReplicationError::MessageTooLarge)?;
        self.u32(len)?;
        self.push(value)
    }
    pub(super) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

pub(super) struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    pub(super) fn take(&mut self, len: usize) -> Result<&'a [u8], ReplicationError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(ReplicationError::Overflow)?;
        if end > self.bytes.len() {
            return Err(ReplicationError::TruncatedFrame);
        }
        let result = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(result)
    }
    pub(super) fn u8(&mut self) -> Result<u8, ReplicationError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(ReplicationError::TruncatedFrame)
    }
    pub(super) fn u16(&mut self) -> Result<u16, ReplicationError> {
        Ok(u16::from_be_bytes(self.take(2).and_then(|bytes| {
            bytes
                .try_into()
                .map_err(|_| ReplicationError::TruncatedFrame)
        })?))
    }
    pub(super) fn u32(&mut self) -> Result<u32, ReplicationError> {
        Ok(u32::from_be_bytes(self.take(4).and_then(|bytes| {
            bytes
                .try_into()
                .map_err(|_| ReplicationError::TruncatedFrame)
        })?))
    }
    pub(super) fn u64(&mut self) -> Result<u64, ReplicationError> {
        Ok(u64::from_be_bytes(self.take(8).and_then(|bytes| {
            bytes
                .try_into()
                .map_err(|_| ReplicationError::TruncatedFrame)
        })?))
    }
    pub(super) fn fixed(&mut self) -> Result<[u8; 32], ReplicationError> {
        self.take(32).and_then(|bytes| {
            bytes
                .try_into()
                .map_err(|_| ReplicationError::TruncatedFrame)
        })
    }
    pub(super) fn bytes(&mut self, max: usize) -> Result<Vec<u8>, ReplicationError> {
        let len = usize::try_from(self.u32()?).map_err(|_| ReplicationError::Overflow)?;
        if len > max || len > self.bytes.len().saturating_sub(self.offset) {
            return Err(ReplicationError::MessageTooLarge);
        }
        Ok(self.take(len)?.to_vec())
    }
    pub(super) fn count(&mut self, max: usize) -> Result<usize, ReplicationError> {
        let count = usize::try_from(self.u32()?).map_err(|_| ReplicationError::Overflow)?;
        if count > max {
            return Err(ReplicationError::MessageTooLarge);
        }
        Ok(count)
    }
    pub(super) fn finish(&self) -> Result<(), ReplicationError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(ReplicationError::TrailingFrame)
        }
    }
}

pub(super) fn write_context(
    writer: &mut Writer,
    context: IdContext,
) -> Result<(), ReplicationError> {
    writer.u8(context.class())?;
    writer.u8(context.domain())?;
    writer.u16(context.ty())?;
    writer.u8(context.version())
}

pub(super) fn read_context(reader: &mut Reader<'_>) -> Result<IdContext, ReplicationError> {
    Ok(IdContext::new(
        reader.u8()?,
        reader.u8()?,
        reader.u16()?,
        reader.u8()?,
    ))
}

pub(super) fn write_identity(
    writer: &mut Writer,
    identity: WireIdentity,
) -> Result<(), ReplicationError> {
    writer.fixed(&identity.as_bytes())?;
    write_context(writer, identity.context())
}

pub(super) fn read_identity(reader: &mut Reader<'_>) -> Result<WireIdentity, ReplicationError> {
    let bytes = reader.fixed()?;
    WireIdentity::from_wire(&bytes, read_context(reader)?).map_err(ReplicationError::from)
}

pub(super) fn write_workspace(
    writer: &mut Writer,
    root: WorkspaceRootClaim,
) -> Result<(), ReplicationError> {
    writer.fixed(&root.as_bytes())
}

pub(super) fn read_workspace(
    reader: &mut Reader<'_>,
) -> Result<WorkspaceRootClaim, ReplicationError> {
    Ok(WorkspaceRootClaim::from_bytes(reader.fixed()?))
}

pub(super) fn write_wire_id<K>(
    writer: &mut Writer,
    id: crate::WireId<K>,
) -> Result<(), ReplicationError> {
    writer.fixed(id.as_bytes())?;
    write_context(writer, id.context())
}

pub(super) fn read_wire_id<K>(
    reader: &mut Reader<'_>,
) -> Result<crate::WireId<K>, ReplicationError> {
    let bytes = reader.fixed()?;
    crate::WireId::from_wire(&bytes, read_context(reader)?).map_err(ReplicationError::from)
}

pub(super) fn write_authority(
    writer: &mut Writer,
    authority: WireAuthority,
) -> Result<(), ReplicationError> {
    write_identity(writer, authority.id)?;
    writer.u64(authority.epoch.0)
}

pub(super) fn read_authority(reader: &mut Reader<'_>) -> Result<WireAuthority, ReplicationError> {
    Ok(AuthorityClaim {
        id: read_identity(reader)?,
        epoch: AuthorityEpoch(reader.u64()?),
    })
}

pub(super) fn write_authority_policy(
    writer: &mut Writer,
    policy: WireAuthorityPolicy,
) -> Result<(), ReplicationError> {
    write_identity(writer, policy.id)?;
    writer.u64(policy.minimum_epoch.0)?;
    writer.u64(policy.revocation_version.0)
}

pub(super) fn read_authority_policy(
    reader: &mut Reader<'_>,
) -> Result<WireAuthorityPolicy, ReplicationError> {
    Ok(WireAuthorityPolicy {
        id: read_identity(reader)?,
        minimum_epoch: AuthorityEpoch(reader.u64()?),
        revocation_version: crate::RevocationVersion(reader.u64()?),
    })
}

pub(super) fn write_coverage(
    writer: &mut Writer,
    coverage: &SparseCoverage,
) -> Result<(), ReplicationError> {
    let count =
        u32::try_from(coverage.ranges().len()).map_err(|_| ReplicationError::CoverageLimit)?;
    writer.u32(count)?;
    for range in coverage.ranges() {
        writer.u64(range.start)?;
        writer.u64(range.len)?;
    }
    Ok(())
}

pub(super) fn read_coverage(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
    bound: Option<u64>,
) -> Result<SparseCoverage, ReplicationError> {
    let count = reader.count(limits.max_ranges)?;
    let mut ranges = Vec::with_capacity(count);
    for _ in 0..count {
        let range = ByteRange::new(reader.u64()?, reader.u64()?)?;
        if bound.is_some_and(|end| range.end().is_ok_and(|range_end| range_end > end)) {
            return Err(ReplicationError::Range);
        }
        ranges.push(range);
    }
    SparseCoverage::from_ranges(ranges, limits.max_ranges)
}

pub(super) fn write_version_range(
    writer: &mut Writer,
    range: VersionRange,
) -> Result<(), ReplicationError> {
    writer.u16(range.min)?;
    writer.u16(range.max)
}

pub(super) fn read_version_range(
    reader: &mut Reader<'_>,
) -> Result<VersionRange, ReplicationError> {
    VersionRange::new(reader.u16()?, reader.u16()?)
}
