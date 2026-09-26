use blake3::Hasher;
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use backend_execution::WorkKey;

use super::identity::{ID_BYTES, PublicationRootId, RawArchiveObjectId, now_millis};

pub(super) static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);
/// A durable lease/fence key. The token is generated privately and is needed
/// for release, so one process can never delete another process's lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquisitionLease {
    /// Effect key being owned.
    pub key: WorkKey,
    /// Private owner token.
    pub token: [u8; ID_BYTES],
    /// Lease expiry in wall-clock milliseconds.
    pub expires_at_millis: u64,
}

/// Outcome of cross-process first-writer-wins object admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CasAdmission {
    /// This writer created the object.
    Winner,
    /// Another writer won; the caller may use the existing object.
    Existing,
}

/// A private temporary artifact whose drop only removes its own name.
pub struct PrivateTemp {
    path: PathBuf,
    file: Option<File>,
}

impl fmt::Debug for PrivateTemp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateTemp")
            .field("path", &self.path)
            .finish()
    }
}

impl PrivateTemp {
    /// Returns the private temporary path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Writes bytes through the private descriptor.
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("temp closed"))?
            .write_all(bytes)
    }
    /// Flushes and closes the temporary descriptor.
    pub fn sync_close(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
            file.sync_all()?;
        }
        Ok(())
    }
}

impl Drop for PrivateTemp {
    fn drop(&mut self) {
        let _ = self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

/// Durable effect ownership and publication fences.
#[derive(Clone, Debug)]
pub struct LeaseStore {
    root: Arc<PathBuf>,
    nonce: Arc<Mutex<u64>>,
}

impl LeaseStore {
    /// Opens/creates a private acquisition root.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("leases"))?;
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("temps"))?;
        Ok(Self {
            root: Arc::new(root),
            nonce: Arc::new(Mutex::new(now_millis() ^ u64::from(std::process::id()))),
        })
    }

    fn next_token(&self) -> [u8; ID_BYTES] {
        let mut nonce = self
            .nonce
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *nonce = nonce.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut hasher = Hasher::new();
        hasher.update(b"backend.acquisition.fence.v1\0");
        hasher.update(&nonce.to_be_bytes());
        hasher.update(&TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        hasher.update(&std::process::id().to_be_bytes());
        *hasher.finalize().as_bytes()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Acquires a durable lease using create-new first-writer admission.
    pub fn acquire(&self, key: WorkKey, ttl: Duration) -> io::Result<Option<LeaseGuard>> {
        let token = self.next_token();
        let path = self.root.join("leases").join(Self::hex(key.as_bytes()));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let expired = fs::read(&path).ok().and_then(|bytes| {
                    if bytes.len() < ID_BYTES + 8 {
                        return Some(true);
                    }
                    let mut expiry = [0_u8; 8];
                    expiry.copy_from_slice(&bytes[ID_BYTES..ID_BYTES + 8]);
                    Some(u64::from_be_bytes(expiry) <= now_millis())
                }) == Some(true);
                if expired {
                    let _ = fs::remove_file(&path);
                    match OpenOptions::new().write(true).create_new(true).open(&path) {
                        Ok(file) => file,
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    return Ok(None);
                }
            }
            Err(error) => return Err(error),
        };
        let expires = now_millis().saturating_add(ttl.as_millis() as u64);
        file.write_all(&token)?;
        file.write_all(&expires.to_be_bytes())?;
        file.sync_all()?;
        Ok(Some(LeaseGuard {
            store: self.clone(),
            lease: AcquisitionLease {
                key,
                token,
                expires_at_millis: expires,
            },
            path,
        }))
    }

    /// Allocates a private randomised temporary name.
    pub fn temp(&self, suffix: &str) -> io::Result<PrivateTemp> {
        let token = self.next_token();
        let path = self
            .root
            .join("temps")
            .join(format!("{}.{}", Self::hex(&token), suffix));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(PrivateTemp {
            path,
            file: Some(file),
        })
    }

    /// Performs first-writer-wins CAS admission of one complete temp object.
    pub fn cas_admit(
        &self,
        temp: &mut PrivateTemp,
        object: RawArchiveObjectId,
    ) -> io::Result<CasAdmission> {
        temp.sync_close()?;
        let target = self.root.join("objects").join(Self::hex(object.as_bytes()));
        match fs::hard_link(temp.path(), &target) {
            Ok(()) => Ok(CasAdmission::Winner),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Ok(CasAdmission::Existing)
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                let mut source = File::open(temp.path())?;
                let destination = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target);
                match destination {
                    Ok(mut destination) => {
                        io::copy(&mut source, &mut destination)?;
                        destination.sync_all()?;
                        Ok(CasAdmission::Winner)
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        Ok(CasAdmission::Existing)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Returns the durable object path after CAS admission.
    #[must_use]
    pub fn object_path(&self, object: RawArchiveObjectId) -> PathBuf {
        self.root.join("objects").join(Self::hex(object.as_bytes()))
    }

    /// Opens the atomic publication pointer.
    pub fn publisher(&self) -> RootPublisher {
        RootPublisher {
            store: self.clone(),
        }
    }
}

/// Affine durable lease guard.
pub struct LeaseGuard {
    store: LeaseStore,
    lease: AcquisitionLease,
    path: PathBuf,
}

impl LeaseGuard {
    /// Returns this lease's fence.
    #[must_use]
    pub const fn lease(&self) -> AcquisitionLease {
        self.lease
    }
    /// Returns whether this lease is still the file owner.
    #[must_use]
    pub fn owns(&self) -> bool {
        fs::read(&self.path).is_ok_and(|bytes| bytes.starts_with(&self.lease.token))
    }

    /// Extends a lease only while its fence still owns the durable record.
    pub fn renew(&mut self, ttl: Duration) -> io::Result<bool> {
        if !self.owns() {
            return Ok(false);
        }
        let expires = now_millis().saturating_add(ttl.as_millis() as u64);
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        file.write_all(&self.lease.token)?;
        file.write_all(&expires.to_be_bytes())?;
        file.sync_all()?;
        self.lease.expires_at_millis = expires;
        Ok(true)
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if self.owns() {
            let _ = fs::remove_file(&self.path);
        }
        let _ = &self.store;
    }
}

/// Atomic old-or-new root pointer publication.
#[derive(Clone, Debug)]
pub struct RootPublisher {
    store: LeaseStore,
}

impl RootPublisher {
    /// Publishes one root pointer by fsync then rename. Recovery can observe
    /// only the previous complete pointer or this complete new pointer.
    pub fn publish(&self, root: PublicationRootId) -> io::Result<()> {
        let mut temp = self.store.temp("root")?;
        temp.write_all(root.as_bytes())?;
        temp.sync_close()?;
        let pointer = self.store.root.join("root.pointer");
        fs::rename(temp.path(), &pointer)?;
        Ok(())
    }
    /// Reads the selected root pointer.
    pub fn read(&self) -> io::Result<Option<PublicationRootId>> {
        match fs::read(self.store.root.join("root.pointer")) {
            Ok(bytes) if bytes.len() == ID_BYTES => {
                let mut root = [0; ID_BYTES];
                root.copy_from_slice(&bytes);
                Ok(Some(PublicationRootId::from_encoded(root)))
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid root pointer",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}
