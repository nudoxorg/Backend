use blake3::Hasher;
use std::{
    fmt,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use backend_execution::WorkKey;

use super::identity::{ID_BYTES, PublicationRootId, RawArchiveObjectId, now_millis};

pub(super) static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);
const GATE_LOCK_ATTEMPTS: usize = 32;
const GATE_LOCK_RETRY: Duration = Duration::from_millis(1);
const LEASE_RECORD_MAGIC: &[u8; 8] = b"ACQLSE01";
const LEASE_RECORD_BODY_BYTES: usize = 8 + 8 + 1 + ID_BYTES + 8;
const LEASE_RECORD_BYTES: usize = LEASE_RECORD_BODY_BYTES + 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LeaseRecord {
    generation: u64,
    slot: Option<u8>,
    active: bool,
    token: [u8; ID_BYTES],
    expires_at_millis: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LeaseSlotRead {
    Missing,
    Invalid,
    Valid(LeaseRecord),
}
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

/// Outcome of compatibility first-writer-wins object admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CasAdmission {
    /// This writer created the object.
    Winner,
    /// Another writer published the same complete object.
    Existing,
}

/// A private temporary artifact whose drop only removes its own name.
pub struct PrivateTemp {
    path: PathBuf,
    file: Option<File>,
}

impl fmt::Debug for PrivateTemp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateTemp")
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
}

impl LeaseStore {
    /// Opens/creates a private acquisition root.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("leases"))?;
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("temps"))?;
        let root = fs::canonicalize(root)?;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    fn next_token() -> io::Result<[u8; ID_BYTES]> {
        let counter = TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut entropy = [0_u8; ID_BYTES];
        #[cfg(unix)]
        File::open("/dev/urandom")?.read_exact(&mut entropy)?;
        #[cfg(windows)]
        backend_platform::win32::random::fill(&mut entropy)?;
        #[cfg(not(any(unix, windows)))]
        entropy[..16].copy_from_slice(&timestamp.to_be_bytes());
        let mut hasher = Hasher::new();
        hasher.update(b"backend.acquisition.fence.v1\0");
        hasher.update(&entropy);
        hasher.update(&timestamp.to_be_bytes());
        hasher.update(&counter.to_be_bytes());
        hasher.update(&std::process::id().to_be_bytes());
        Ok(*hasher.finalize().as_bytes())
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    const fn stripe(key: WorkKey) -> u8 {
        key.as_bytes()[0]
    }

    fn gate_path_for_stripe(&self, stripe: u8) -> PathBuf {
        // A fixed 256-stripe set bounds coordination files while allowing
        // unrelated keys to publish independently in most cases.
        self.root
            .join("leases")
            .join(format!(".coordination-{stripe:02x}.lock"))
    }

    /// Briefly serializes one key stripe's record compare/update across
    /// processes. It retries short contention for a bounded interval and is
    /// never retained by a LeaseGuard or across network work.
    fn try_gate(&self, key: WorkKey) -> io::Result<Option<File>> {
        Self::try_lock_bounded(self.open_gate_file(key)?)
    }

    /// Locks every distinct stripe in ascending order. This order is shared by
    /// all callers, so a two-key publication cannot deadlock against another
    /// publication which names the same stripes in reverse order.
    fn try_gates(&self, keys: impl IntoIterator<Item = WorkKey>) -> io::Result<Option<Vec<File>>> {
        let mut stripes: Vec<_> = keys.into_iter().map(Self::stripe).collect();
        stripes.sort_unstable();
        stripes.dedup();

        let mut gates = Vec::with_capacity(stripes.len());
        for stripe in stripes {
            let Some(gate) = Self::try_lock_bounded(self.open_gate_file_for_stripe(stripe)?)?
            else {
                return Ok(None);
            };
            gates.push(gate);
        }
        Ok(Some(gates))
    }

    fn try_lock_bounded(file: File) -> io::Result<Option<File>> {
        for attempt in 0..GATE_LOCK_ATTEMPTS {
            match file.try_lock() {
                Ok(()) => return Ok(Some(file)),
                Err(TryLockError::WouldBlock) if attempt + 1 < GATE_LOCK_ATTEMPTS => {
                    thread::sleep(GATE_LOCK_RETRY);
                }
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
        Ok(None)
    }

    fn open_gate_file(&self, key: WorkKey) -> io::Result<File> {
        self.open_gate_file_for_stripe(Self::stripe(key))
    }

    fn open_gate_file_for_stripe(&self, stripe: u8) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.gate_path_for_stripe(stripe))
    }

    fn lease_slot_path(&self, key: WorkKey, slot: u8) -> PathBuf {
        self.root
            .join("leases")
            .join(format!("{}.lease.{slot}", Self::hex(key.as_bytes())))
    }

    fn legacy_lease_path(&self, key: WorkKey) -> PathBuf {
        self.root.join("leases").join(Self::hex(key.as_bytes()))
    }

    fn lease_temp_path(&self, key: WorkKey) -> PathBuf {
        self.root
            .join("leases")
            .join(format!(".{}.lease.tmp", Self::hex(key.as_bytes())))
    }

    fn ttl_millis(ttl: Duration) -> io::Result<u64> {
        let millis = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        if millis == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "lease TTL must be at least one millisecond",
            ));
        }
        Ok(millis)
    }

    fn encode_lease_record(record: LeaseRecord) -> [u8; LEASE_RECORD_BYTES] {
        let mut bytes = [0_u8; LEASE_RECORD_BYTES];
        bytes[..8].copy_from_slice(LEASE_RECORD_MAGIC);
        bytes[8..16].copy_from_slice(&record.generation.to_be_bytes());
        bytes[16] = u8::from(record.active);
        bytes[17..17 + ID_BYTES].copy_from_slice(&record.token);
        let expiry_offset = 17 + ID_BYTES;
        bytes[expiry_offset..expiry_offset + 8]
            .copy_from_slice(&record.expires_at_millis.to_be_bytes());
        let digest = blake3::hash(&bytes[..LEASE_RECORD_BODY_BYTES]);
        bytes[LEASE_RECORD_BODY_BYTES..].copy_from_slice(digest.as_bytes());
        bytes
    }

    fn decode_lease_record(bytes: &[u8], slot: u8) -> Option<LeaseRecord> {
        if bytes.len() != LEASE_RECORD_BYTES || &bytes[..8] != LEASE_RECORD_MAGIC {
            return None;
        }
        let digest = blake3::hash(&bytes[..LEASE_RECORD_BODY_BYTES]);
        if &bytes[LEASE_RECORD_BODY_BYTES..] != digest.as_bytes() {
            return None;
        }
        let generation = u64::from_be_bytes(bytes[8..16].try_into().ok()?);
        if generation == 0 {
            return None;
        }
        let active = match bytes[16] {
            0 => false,
            1 => true,
            _ => return None,
        };
        let mut token = [0; ID_BYTES];
        token.copy_from_slice(&bytes[17..17 + ID_BYTES]);
        let expiry_offset = 17 + ID_BYTES;
        let expires_at_millis =
            u64::from_be_bytes(bytes[expiry_offset..expiry_offset + 8].try_into().ok()?);
        Some(LeaseRecord {
            generation,
            slot: Some(slot),
            active,
            token,
            expires_at_millis,
        })
    }

    fn read_fixed(file: &mut File, expected_len: usize) -> io::Result<Option<Vec<u8>>> {
        let mut bytes = vec![0_u8; expected_len + 1];
        let mut read = 0;
        while read < bytes.len() {
            let count = file.read(&mut bytes[read..])?;
            if count == 0 {
                break;
            }
            read += count;
        }
        if read != expected_len {
            return Ok(None);
        }
        bytes.truncate(read);
        Ok(Some(bytes))
    }

    fn read_slot(&self, key: WorkKey, slot: u8) -> io::Result<LeaseSlotRead> {
        let path = self.lease_slot_path(key, slot);
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LeaseSlotRead::Missing);
            }
            Err(error) => return Err(error),
        };
        let Some(bytes) = Self::read_fixed(&mut file, LEASE_RECORD_BYTES)? else {
            return Ok(LeaseSlotRead::Invalid);
        };
        Ok(Self::decode_lease_record(&bytes, slot)
            .map_or(LeaseSlotRead::Invalid, LeaseSlotRead::Valid))
    }

    fn read_legacy_record(&self, key: WorkKey) -> io::Result<LeaseSlotRead> {
        let path = self.legacy_lease_path(key);
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LeaseSlotRead::Missing);
            }
            Err(error) => return Err(error),
        };
        let Some(bytes) = Self::read_fixed(&mut file, ID_BYTES + 8)? else {
            return Ok(LeaseSlotRead::Invalid);
        };
        let mut token = [0; ID_BYTES];
        token.copy_from_slice(&bytes[..ID_BYTES]);
        let expires_at_millis = u64::from_be_bytes(bytes[ID_BYTES..].try_into().expect("8 bytes"));
        Ok(LeaseSlotRead::Valid(LeaseRecord {
            generation: 0,
            slot: None,
            active: true,
            token,
            expires_at_millis,
        }))
    }

    /// Selects the highest fully valid slot. If a crash tears an update to
    /// the older slot, the other slot remains the last committed record.
    /// The fixed orphan temp is never considered a lease and is overwritten
    /// by the next publisher.
    fn latest_lease(&self, key: WorkKey) -> io::Result<Option<LeaseRecord>> {
        let first = self.read_slot(key, 0)?;
        let second = self.read_slot(key, 1)?;
        let slot_record = match (first, second) {
            (LeaseSlotRead::Valid(left), LeaseSlotRead::Valid(right)) => {
                match left.generation.cmp(&right.generation) {
                    std::cmp::Ordering::Greater => Some(left),
                    std::cmp::Ordering::Less => Some(right),
                    std::cmp::Ordering::Equal if left == right => Some(left),
                    std::cmp::Ordering::Equal => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "conflicting acquisition lease generations",
                        ));
                    }
                }
            }
            (LeaseSlotRead::Valid(record), _) | (_, LeaseSlotRead::Valid(record)) => Some(record),
            (LeaseSlotRead::Missing, LeaseSlotRead::Missing) => None,
            _ => None,
        };
        if slot_record.is_some() {
            return Ok(slot_record);
        }

        match self.read_legacy_record(key)? {
            LeaseSlotRead::Valid(record) => Ok(Some(record)),
            LeaseSlotRead::Missing
                if first == LeaseSlotRead::Missing && second == LeaseSlotRead::Missing =>
            {
                Ok(None)
            }
            LeaseSlotRead::Missing | LeaseSlotRead::Invalid => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no valid acquisition lease record",
            )),
        }
    }

    fn publish_lease_record(
        &self,
        key: WorkKey,
        previous: Option<LeaseRecord>,
        active: bool,
        token: [u8; ID_BYTES],
        expires_at_millis: u64,
    ) -> io::Result<LeaseRecord> {
        let (slot, generation) = match previous {
            Some(previous) => (
                previous.slot.map_or(0, |slot| 1 - slot),
                previous.generation.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "lease generation overflow")
                })?,
            ),
            None => (0, 1),
        };
        let record = LeaseRecord {
            generation,
            slot: Some(slot),
            active,
            token,
            expires_at_millis,
        };
        let bytes = Self::encode_lease_record(record);
        let temp_path = self.lease_temp_path(key);
        let mut temp = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)?;
        temp.write_all(&bytes)?;
        temp.sync_all()?;
        drop(temp);

        let target = self.lease_slot_path(key, slot);
        // If interrupted before rename, this orphan temp is ignored and the
        // other slot remains the current complete record.
        match fs::remove_file(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // This is a create-only rename: the previous newest slot remains intact
        // while the older slot is removed and replaced, avoiding overwrite
        // semantics that differ across platforms.
        fs::rename(&temp_path, &target)?;
        backend_platform::durability::open_directory(&self.root.join("leases"))?.sync_all()?;
        let _ = fs::remove_file(temp_path);
        Ok(record)
    }

    /// Acquires a durable lease by atomically comparing/updating its record
    /// under the interprocess coordination lock. An expired record is taken
    /// over only while that lock is held.
    pub fn acquire(&self, key: WorkKey, ttl: Duration) -> io::Result<Option<LeaseGuard>> {
        let ttl_millis = Self::ttl_millis(ttl)?;
        let Some(_gate) = self.try_gate(key)? else {
            return Ok(None);
        };
        let previous = self.latest_lease(key)?;
        if previous.is_some_and(|record| record.active && record.expires_at_millis > now_millis()) {
            return Ok(None);
        }
        let token = Self::next_token()?;
        let expires = now_millis().saturating_add(ttl_millis);
        self.publish_lease_record(key, previous, true, token, expires)?;
        if expires <= now_millis() {
            return Ok(None);
        }
        drop(_gate);
        Ok(Some(LeaseGuard {
            store: self.clone(),
            lease: AcquisitionLease {
                key,
                token,
                expires_at_millis: expires,
            },
        }))
    }

    /// Allocates a private, synced-on-close temporary name.
    pub fn temp(&self, suffix: &str) -> io::Result<PrivateTemp> {
        let token = Self::next_token()?;
        let safe_suffix: String = suffix
            .bytes()
            .filter(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            .map(char::from)
            .collect();
        let suffix = if safe_suffix.is_empty() {
            "object"
        } else {
            &safe_suffix
        };
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

    /// Performs compatibility CAS admission of one complete temporary.
    /// Registry production uses the sealed content-addressed store instead.
    pub fn cas_admit(
        &self,
        temp: &mut PrivateTemp,
        object: RawArchiveObjectId,
    ) -> io::Result<CasAdmission> {
        temp.sync_close()?;
        let target = self.root.join("objects").join(Self::hex(object.as_bytes()));
        let parent = target.parent().expect("object parent");
        // Copy into a unique temp on the target filesystem and publish only a
        // hard link. This keeps caller-held temp paths from mutating the CAS
        // inode after publication and avoids partial final names on EXDEV.
        let stage = parent.join(format!(".{}.stage", Self::hex(&Self::next_token()?)));
        let result = (|| {
            let mut source = backend_platform::durability::open_regular_file_nofollow(temp.path())?;
            let mut staged = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&stage)?;
            io::copy(&mut source, &mut staged)?;
            staged.sync_all()?;
            drop(staged);
            let admission = match fs::hard_link(&stage, &target) {
                Ok(()) => CasAdmission::Winner,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    verify_same_file_contents(&stage, &target)?;
                    CasAdmission::Existing
                }
                Err(error) => return Err(error),
            };
            // The directory barrier also applies when a verified object was
            // already present, since its name may have been published shortly
            // before a crash and the prior writer may not have completed it.
            sync_directory(parent)?;
            Ok(admission)
        })();
        let _ = fs::remove_file(stage);
        result
    }

    /// Returns the compatibility CAS path for one object identity.
    #[must_use]
    pub fn object_path(&self, object: RawArchiveObjectId) -> PathBuf {
        self.root.join("objects").join(Self::hex(object.as_bytes()))
    }

    /// Opens the legacy atomic root-pointer adapter.
    pub fn publisher(&self) -> RootPublisher {
        RootPublisher {
            store: self.clone(),
        }
    }
}

/// Atomic old-or-new root pointer publication retained for source
/// compatibility. Product acquisition uses `AcquisitionReceiptStore`.
#[derive(Clone, Debug)]
pub struct RootPublisher {
    store: LeaseStore,
}

impl RootPublisher {
    /// Publishes one root pointer by fsync then atomic replacement.
    pub fn publish(&self, root: PublicationRootId) -> io::Result<()> {
        let mut temp = self.store.temp("root")?;
        temp.write_all(root.as_bytes())?;
        temp.sync_close()?;
        let pointer = self.store.root.join("root.pointer");
        backend_platform::durable::replace_file(temp.path(), &pointer)?;
        sync_directory(&self.store.root)
    }

    /// Reads the selected root pointer.
    pub fn read(&self) -> io::Result<Option<PublicationRootId>> {
        let pointer = self.store.root.join("root.pointer");
        match backend_platform::durability::open_regular_file_nofollow(&pointer) {
            Ok(mut file) => {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                if bytes.len() == ID_BYTES {
                    let mut root = [0; ID_BYTES];
                    root.copy_from_slice(&bytes);
                    Ok(Some(PublicationRootId::from_encoded(root)))
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid root pointer",
                    ))
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn verify_same_file_contents(first: &Path, second: &Path) -> io::Result<()> {
    let first_meta = fs::symlink_metadata(first)?;
    let second_meta = fs::symlink_metadata(second)?;
    if !first_meta.file_type().is_file()
        || !second_meta.file_type().is_file()
        || first_meta.len() != second_meta.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed object collision",
        ));
    }
    let first_bytes = fs::read(first)?;
    let mut second_file = backend_platform::durability::open_regular_file_nofollow(second)?;
    let mut second_bytes = Vec::new();
    second_file.read_to_end(&mut second_bytes)?;
    if first_bytes == second_bytes {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed object collision",
        ))
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    backend_platform::durability::open_directory(path)?.sync_all()
}

/// Affine durable lease guard.
pub struct LeaseGuard {
    store: LeaseStore,
    lease: AcquisitionLease,
}

impl LeaseGuard {
    /// Returns this lease's fence.
    #[must_use]
    pub const fn lease(&self) -> AcquisitionLease {
        self.lease
    }
    /// Returns whether this exact fence still owns an unexpired lease record.
    #[must_use]
    pub fn owns(&self) -> bool {
        let Ok(Some(_gate)) = self.store.try_gate(self.lease.key) else {
            return false;
        };
        self.store.latest_lease(self.lease.key).is_ok_and(|record| {
            record.is_some_and(|record| {
                record.active
                    && record.token == self.lease.token
                    && record.expires_at_millis > now_millis()
            })
        })
    }

    /// Extends a lease only while its exact fence owns an unexpired record.
    pub fn renew(&mut self, ttl: Duration) -> io::Result<bool> {
        let ttl_millis = LeaseStore::ttl_millis(ttl)?;
        let Some(_gate) = self.store.try_gate(self.lease.key)? else {
            return Ok(false);
        };
        let Some(previous) = self.store.latest_lease(self.lease.key)? else {
            return Ok(false);
        };
        if !previous.active
            || previous.token != self.lease.token
            || previous.expires_at_millis <= now_millis()
        {
            return Ok(false);
        }
        let expires = now_millis().saturating_add(ttl_millis);
        self.store.publish_lease_record(
            self.lease.key,
            Some(previous),
            true,
            self.lease.token,
            expires,
        )?;
        self.lease.expires_at_millis = expires;
        Ok(expires > now_millis())
    }

    /// Runs a short durable publication only while this exact, unexpired
    /// fence is current. The callback executes under interprocess coordination
    /// so takeover cannot race its commit. Keep the callback filesystem-only;
    /// network work belongs before this call. An expired lease fails closed.
    pub fn publish_if_current<T>(
        &mut self,
        ttl: Duration,
        publish: impl FnOnce(AcquisitionLease) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        let ttl_millis = LeaseStore::ttl_millis(ttl)?;
        let Some(gate) = self.store.try_gate(self.lease.key)? else {
            return Ok(None);
        };
        let Some(previous) = self.store.latest_lease(self.lease.key)? else {
            return Ok(None);
        };
        if !previous.active
            || previous.token != self.lease.token
            || previous.expires_at_millis <= now_millis()
        {
            return Ok(None);
        }
        let expires = now_millis().saturating_add(ttl_millis);
        if expires <= now_millis() {
            return Ok(None);
        }
        self.store.publish_lease_record(
            self.lease.key,
            Some(previous),
            true,
            self.lease.token,
            expires,
        )?;
        if expires <= now_millis() {
            return Ok(None);
        }
        self.lease.expires_at_millis = expires;
        let result = publish(self.lease)?;
        drop(gate);
        Ok(Some(result))
    }

    /// Runs one short filesystem publication only while both exact, unexpired
    /// fences are current. Distinct stripes are acquired once in sorted order;
    /// both records are checked and durably renewed before the callback runs.
    /// Keep the callback filesystem-only: network work belongs before this
    /// call. A stale, missing, expired, or briefly contended fence fails closed.
    pub fn publish_if_both_current<T>(
        &mut self,
        other: &mut LeaseGuard,
        ttl: Duration,
        publish: impl FnOnce(AcquisitionLease, AcquisitionLease) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        let ttl_millis = LeaseStore::ttl_millis(ttl)?;
        if self.store.root.as_path() != other.store.root.as_path() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "paired acquisition fences must share one coordination root",
            ));
        }

        let Some(gates) = self.store.try_gates([self.lease.key, other.lease.key])? else {
            return Ok(None);
        };
        let Some(first_previous) = self.store.latest_lease(self.lease.key)? else {
            return Ok(None);
        };
        let Some(second_previous) = self.store.latest_lease(other.lease.key)? else {
            return Ok(None);
        };
        let now = now_millis();
        if !first_previous.active
            || first_previous.token != self.lease.token
            || first_previous.expires_at_millis <= now
            || !second_previous.active
            || second_previous.token != other.lease.token
            || second_previous.expires_at_millis <= now
        {
            return Ok(None);
        }
        if self.lease.key == other.lease.key && self.lease.token != other.lease.token {
            return Ok(None);
        }

        let expires = now.saturating_add(ttl_millis);
        if expires <= now_millis() {
            return Ok(None);
        }

        // One guard per key is the normal case. If both handles represent the
        // same exact lease, write that key once and refresh both local copies.
        if self.lease.key == other.lease.key {
            self.store.publish_lease_record(
                self.lease.key,
                Some(first_previous),
                true,
                self.lease.token,
                expires,
            )?;
        } else if self.lease.key.as_bytes() < other.lease.key.as_bytes() {
            self.store.publish_lease_record(
                self.lease.key,
                Some(first_previous),
                true,
                self.lease.token,
                expires,
            )?;
            self.store.publish_lease_record(
                other.lease.key,
                Some(second_previous),
                true,
                other.lease.token,
                expires,
            )?;
        } else {
            self.store.publish_lease_record(
                other.lease.key,
                Some(second_previous),
                true,
                other.lease.token,
                expires,
            )?;
            self.store.publish_lease_record(
                self.lease.key,
                Some(first_previous),
                true,
                self.lease.token,
                expires,
            )?;
        }
        if expires <= now_millis() {
            return Ok(None);
        }
        self.lease.expires_at_millis = expires;
        other.lease.expires_at_millis = expires;
        let result = publish(self.lease, other.lease)?;
        drop(gates);
        Ok(Some(result))
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Ok(Some(_gate)) = self.store.try_gate(self.lease.key) {
            if let Ok(Some(current)) = self.store.latest_lease(self.lease.key)
                && current.active
                && current.token == self.lease.token
            {
                // A durable released generation prevents an older active slot
                // from becoming current if cleanup or a later write crashes.
                let _ = self.store.publish_lease_record(
                    self.lease.key,
                    Some(current),
                    false,
                    self.lease.token,
                    0,
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "lease_tests.rs"]
mod tests;
