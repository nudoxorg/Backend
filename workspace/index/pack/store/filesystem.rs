//! A filesystem-backed [`ObjectPackStore`] sharded by id hex prefix.
//!
//! Packs live at `<root>/<aa>/<full-hex>.ndpk`, where `aa` is the first byte of
//! the id in lower hex. Sharding keeps directory fan-out bounded. Writes are
//! atomic (write to a temp file in the shard dir, then rename) so a crashed
//! `put_pack` never leaves a half-written pack readable.

use std::ops::Range;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use heart::object_pack::{MemberKey, ObjectPackId};

use crate::pack::builder::ObjectPackBuilder;
use crate::pack::error::PackError;
use crate::pack::outboard::{MemberOutboard, OutboardSidecar, OUTBOARD_FILE_EXTENSION};
use crate::pack::reader::ObjectPackReader;

use super::{EndpointId, ObjectPackStore};

/// The on-disk extension for a sealed pack.
const PACK_FILE_EXTENSION: &str = "ndpk";

/// A local, sharded, content-addressed pack store rooted at a directory.
#[derive(Debug, Clone)]
pub struct FilesystemObjectPackStore {
    root: PathBuf,
}

impl FilesystemObjectPackStore {
    /// Open (creating if absent) a store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PackError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// The root directory of this store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The two-hex-digit shard directory for an id.
    fn shard_directory(&self, id: &ObjectPackId) -> PathBuf {
        let hex = id.0.hex();
        // `hex` is 64 lower-hex chars; the first two are the shard.
        self.root.join(&hex[0..2])
    }

    /// The absolute path of the pack file for an id.
    fn pack_path(&self, id: &ObjectPackId) -> PathBuf {
        let hex = id.0.hex();
        self.shard_directory(id)
            .join(format!("{hex}.{PACK_FILE_EXTENSION}"))
    }

    /// The absolute path of the outboard sidecar file for an id.
    fn outboard_path(&self, id: &ObjectPackId) -> PathBuf {
        let hex = id.0.hex();
        self.shard_directory(id)
            .join(format!("{hex}.{OUTBOARD_FILE_EXTENSION}"))
    }

    /// Read the outboard sidecar for a pack, or an empty sidecar when the file
    /// is absent (a pack with no large members has no sidecar).
    fn read_sidecar(&self, id: &ObjectPackId) -> Result<OutboardSidecar, PackError> {
        let path = self.outboard_path(id);
        match std::fs::read(&path) {
            Ok(bytes) => OutboardSidecar::decode(&bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(OutboardSidecar::default())
            }
            Err(error) => Err(PackError::Io(error)),
        }
    }

    /// Atomically publish `contents` at `final_path` via a uniquely-named temp
    /// file in the same directory, then rename. Rename within a directory is
    /// atomic on POSIX and Windows, so a crash never leaves a half-written file
    /// readable at `final_path`.
    fn atomic_write(
        &self,
        shard: &Path,
        final_path: &Path,
        temp_tag: &str,
        contents: &[u8],
    ) -> Result<(), PackError> {
        let temporary_path = shard.join(format!(".{temp_tag}.{}.tmp", std::process::id()));
        let publish = std::fs::write(&temporary_path, contents)
            .and_then(|()| std::fs::rename(&temporary_path, final_path));
        if let Err(error) = publish {
            let _ = std::fs::remove_file(&temporary_path);
            return Err(PackError::Io(error));
        }
        Ok(())
    }

    /// Write the sealed pack bytes and its outboard sidecar into the store,
    /// pack first then sidecar, each atomically. Used by both the local seal
    /// path and the verified-fetch install path.
    fn write_pack_and_sidecar(
        &self,
        id: &ObjectPackId,
        pack_bytes: &[u8],
        outboards: &[MemberOutboard],
    ) -> Result<(), PackError> {
        let shard = self.shard_directory(id);
        std::fs::create_dir_all(&shard)?;

        let hex = id.0.hex();
        self.atomic_write(&shard, &self.pack_path(id), &hex, pack_bytes)?;

        // Write the sidecar only when there is at least one outboard; a pack
        // with no large members needs none. The pack is the atomicity anchor —
        // if the sidecar write fails, the outboard can always be regenerated
        // from the pack, so we surface the error but the pack is already durable.
        let sidecar = OutboardSidecar::from_members(outboards.to_vec());
        if !sidecar.is_empty() {
            let sidecar_bytes = sidecar.encode()?;
            self.atomic_write(
                &shard,
                &self.outboard_path(id),
                &format!("{hex}.ob"),
                &sidecar_bytes,
            )?;
        }
        Ok(())
    }

    /// Open a reader over a stored pack, or [`PackError::MemberNotFound`]-style
    /// absence surfaced as an I/O `NotFound`.
    fn open_reader(&self, id: &ObjectPackId) -> Result<ObjectPackReader, PackError> {
        ObjectPackReader::open_path(&self.pack_path(id))
    }
}

impl ObjectPackStore for FilesystemObjectPackStore {
    fn put_pack(&self, builder: ObjectPackBuilder) -> Result<ObjectPackId, PackError> {
        // Seal with outboards so large members become verified-streamable.
        // The pack bytes and id are identical to a plain seal; the outboards go
        // to a sidecar (never inside the frozen pack).
        let (bytes, id, outboards) = builder.seal_with_outboards()?;
        self.write_pack_and_sidecar(&id, &bytes, &outboards)?;
        Ok(id)
    }

    fn has(&self, id: &ObjectPackId) -> bool {
        self.pack_path(id).is_file()
    }

    fn read_pack_bytes(&self, id: &ObjectPackId) -> Result<Bytes, PackError> {
        let bytes = std::fs::read(self.pack_path(id))?;
        Ok(Bytes::from(bytes))
    }

    fn read_all_outboards(&self, id: &ObjectPackId) -> Result<Vec<MemberOutboard>, PackError> {
        Ok(self.read_sidecar(id)?.members)
    }

    fn outboard(
        &self,
        id: &ObjectPackId,
        key: &MemberKey,
    ) -> Result<Option<MemberOutboard>, PackError> {
        let sidecar = self.read_sidecar(id)?;
        Ok(sidecar.get(key).cloned())
    }

    fn install_pack(
        &self,
        id: &ObjectPackId,
        pack_bytes: &[u8],
        outboards: &[MemberOutboard],
    ) -> Result<(), PackError> {
        // Re-derive the id from the bytes and reject a mismatch: a fetch that
        // served the wrong or a tampered pack must never install under `id`.
        let reader = ObjectPackReader::open_bytes(Bytes::copy_from_slice(pack_bytes))?;
        if reader.id() != *id {
            return Err(PackError::FetchedIdMismatch);
        }
        self.write_pack_and_sidecar(id, pack_bytes, outboards)
    }

    fn get_member_range(
        &self,
        id: &ObjectPackId,
        key: &MemberKey,
        range: Range<u64>,
    ) -> Result<Bytes, PackError> {
        self.open_reader(id)?.get_member_range(key, range)
    }

    fn get_member(&self, id: &ObjectPackId, key: &MemberKey) -> Result<Bytes, PackError> {
        self.open_reader(id)?.get_member(key)
    }

    fn provide_iroh(&self, _id: &ObjectPackId) -> Result<(), PackError> {
        // TODO(transport): wire iroh-blobs + Bao provide (INDEX-PLAN §7.2).
        Err(PackError::TransportNotWired)
    }

    fn fetch_iroh(&self, _id: &ObjectPackId, _from: &EndpointId) -> Result<(), PackError> {
        // TODO(transport): wire iroh-blobs fetch (INDEX-PLAN §7.2).
        Err(PackError::TransportNotWired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use heart::object_pack::RelativePath;

    fn sample_builder() -> ObjectPackBuilder {
        let mut builder = ObjectPackBuilder::new();
        builder
            .add_source_file(
                RelativePath("src/lib.rs".into()),
                Bytes::from_static(b"fn main() {}\n"),
            )
            .expect("add source file");
        builder
    }

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemObjectPackStore::open(dir.path()).expect("open store");
        let id = store.put_pack(sample_builder()).expect("put");
        assert!(store.has(&id));

        let key = MemberKey::Source { path: RelativePath("src/lib.rs".into()) };
        let bytes = store.get_member(&id, &key).expect("get member");
        assert_eq!(&bytes[..], b"fn main() {}\n");
    }

    #[test]
    fn put_is_idempotent_by_determinism() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemObjectPackStore::open(dir.path()).expect("open store");
        let first = store.put_pack(sample_builder()).expect("put");
        let second = store.put_pack(sample_builder()).expect("put again");
        assert_eq!(first, second);
    }

    #[test]
    fn iroh_methods_are_not_wired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemObjectPackStore::open(dir.path()).expect("open store");
        let id = store.put_pack(sample_builder()).expect("put");
        assert!(matches!(store.provide_iroh(&id), Err(PackError::TransportNotWired)));
        assert!(matches!(
            store.fetch_iroh(&id, &EndpointId("peer".into())),
            Err(PackError::TransportNotWired)
        ));
    }
}
