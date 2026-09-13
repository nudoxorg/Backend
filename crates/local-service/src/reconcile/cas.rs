//! Persistent canonical receiving CAS for product closure objects.
//!
//! Staged extents remain private until the canonical digest and exact object
//! identity are checked. Durable commits use an atomic rename so restart and
//! output admission see either the previous object or the complete new object.

use backend_engine::{
    CanonicalCas, ImmutableObjectSchema, ObjectKey, ObjectVersion, ReceivingCasSink,
    ReplicationError, StagedExtent, TransferId,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct ProductReceivingCas {
    sessions: BTreeMap<TransferId, ProductCasSession>,
    committed: BTreeMap<[u8; 32], Arc<[u8]>>,
    durable_root: Option<PathBuf>,
}

impl Default for ProductReceivingCas {
    fn default() -> Self {
        Self {
            sessions: BTreeMap::new(),
            committed: BTreeMap::new(),
            durable_root: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProductCasSession {
    transfer: TransferId,
    key: ObjectKey<ImmutableObjectSchema>,
    version: ObjectVersion<ImmutableObjectSchema>,
    len: u64,
    bytes: BTreeMap<u64, Arc<[u8]>>,
}

impl ProductReceivingCas {
    /// Opens the owner-scoped immutable CAS directory. Committed objects are
    /// represented by atomically renamed files; staged sessions remain
    /// unpublished until their canonical digest passes `commit`.
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self, ReplicationError> {
        let path = path.as_ref();
        fs::create_dir_all(path).map_err(|_| ReplicationError::Disconnected)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path)
                .map_err(|_| ReplicationError::Disconnected)?
                .permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(path, permissions).map_err(|_| ReplicationError::Disconnected)?;
        }
        Ok(Self {
            sessions: BTreeMap::new(),
            committed: BTreeMap::new(),
            durable_root: Some(path.to_owned()),
        })
    }

    pub(super) fn contains(&self, version: [u8; 32]) -> bool {
        self.committed.contains_key(&version)
            || self
                .durable_root
                .as_deref()
                .is_some_and(|root| root.join(hex_digest(version)).is_file())
    }

    fn persist(
        &self,
        version: [u8; 32],
        bytes: &[u8],
        transfer: TransferId,
    ) -> Result<(), ReplicationError> {
        let Some(root) = self.durable_root.as_deref() else {
            return Ok(());
        };
        let target = root.join(hex_digest(version));
        if target.is_file() {
            return Ok(());
        }
        let temporary = root.join(format!(".{:016x}.part", transfer.get()));
        fs::write(&temporary, bytes).map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(&temporary, &target).map_err(|_| {
            let _ = fs::remove_file(&temporary);
            ReplicationError::Disconnected
        })?;
        Ok(())
    }
}

fn hex_digest(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

impl ReceivingCasSink<ImmutableObjectSchema> for ProductReceivingCas {
    type Session = ProductCasSession;
    type Receipt = [u8; 32];

    fn begin(
        &mut self,
        transfer: TransferId,
        key: ObjectKey<ImmutableObjectSchema>,
        version: ObjectVersion<ImmutableObjectSchema>,
        len: u64,
    ) -> Result<Self::Session, ReplicationError> {
        if self.sessions.contains_key(&transfer) {
            return Err(ReplicationError::ReplayConflict);
        }
        Ok(ProductCasSession {
            transfer,
            key,
            version,
            len,
            bytes: BTreeMap::new(),
        })
    }

    fn resume(
        &mut self,
        transfer: TransferId,
        key: ObjectKey<ImmutableObjectSchema>,
        version: ObjectVersion<ImmutableObjectSchema>,
        len: u64,
        checkpoint: &backend_engine::ReceivingCheckpoint<ImmutableObjectSchema>,
    ) -> Result<Self::Session, ReplicationError> {
        let session = self
            .sessions
            .remove(&transfer)
            .ok_or(ReplicationError::Disconnected)?;
        if session.key != key || session.version != version || session.len != len {
            return Err(ReplicationError::IdentityMismatch);
        }
        for extent in &checkpoint.extents {
            if !session.bytes.contains_key(&extent.offset) {
                return Err(ReplicationError::Disconnected);
            }
        }
        Ok(session)
    }

    fn write(
        &mut self,
        session: &mut Self::Session,
        extent: StagedExtent,
        bytes: Arc<[u8]>,
    ) -> Result<(), ReplicationError> {
        if extent.offset.checked_add(extent.len) != Some(session.len)
            || extent.offset != 0
            || bytes.len() as u64 != extent.len
        {
            return Err(ReplicationError::Range);
        }
        if session.bytes.insert(extent.offset, bytes).is_some() {
            return Err(ReplicationError::ReplayConflict);
        }
        self.sessions.insert(session.transfer, session.clone());
        Ok(())
    }

    fn read_extent(
        &mut self,
        session: &mut Self::Session,
        extent: StagedExtent,
        visitor: &mut dyn FnMut(&[u8]) -> Result<(), ReplicationError>,
    ) -> Result<(), ReplicationError> {
        let bytes = session
            .bytes
            .get(&extent.offset)
            .ok_or(ReplicationError::Disconnected)?;
        visitor(bytes)
    }

    fn commit(
        &mut self,
        session: Self::Session,
        key: ObjectKey<ImmutableObjectSchema>,
        version: ObjectVersion<ImmutableObjectSchema>,
        len: u64,
        digest: [u8; 32],
    ) -> Result<Self::Receipt, ReplicationError> {
        self.sessions.remove(&session.transfer);
        if session.key != key || session.version != version || session.len != len {
            return Err(ReplicationError::IdentityMismatch);
        }
        let bytes = session
            .bytes
            .values()
            .next()
            .cloned()
            .ok_or(ReplicationError::Incomplete)?;
        let actual = backend_engine::canonical_object_digest::<ImmutableObjectSchema>(
            bytes.len() as u64,
            [bytes.as_ref()],
        );
        if actual != digest || actual != session.version.to_bytes() {
            return Err(ReplicationError::IdentityMismatch);
        }
        self.persist(actual, bytes.as_ref(), session.transfer)?;
        self.committed.insert(actual, bytes);
        Ok(actual)
    }

    fn abort(&mut self, session: Self::Session) {
        self.sessions.remove(&session.transfer);
    }
}

impl CanonicalCas<ImmutableObjectSchema> for ProductReceivingCas {
    type Session = ProductCasSession;
    type Receipt = [u8; 32];

    fn begin(
        &mut self,
        transfer: TransferId,
        key: ObjectKey<ImmutableObjectSchema>,
        version: ObjectVersion<ImmutableObjectSchema>,
        len: u64,
    ) -> Result<Self::Session, ReplicationError> {
        <Self as ReceivingCasSink<ImmutableObjectSchema>>::begin(self, transfer, key, version, len)
    }

    fn write(
        &mut self,
        session: &mut Self::Session,
        offset: u64,
        bytes: Arc<[u8]>,
    ) -> Result<(), ReplicationError> {
        if offset != 0 || bytes.len() as u64 != session.len {
            return Err(ReplicationError::Range);
        }
        session.bytes.insert(offset, bytes);
        Ok(())
    }

    fn commit(
        &mut self,
        session: Self::Session,
        key: ObjectKey<ImmutableObjectSchema>,
        version: ObjectVersion<ImmutableObjectSchema>,
        len: u64,
        digest: [u8; 32],
    ) -> Result<Self::Receipt, ReplicationError> {
        <Self as ReceivingCasSink<ImmutableObjectSchema>>::commit(
            self, session, key, version, len, digest,
        )
    }

    fn abort(&mut self, session: Self::Session) {
        <Self as ReceivingCasSink<ImmutableObjectSchema>>::abort(self, session);
    }
}
