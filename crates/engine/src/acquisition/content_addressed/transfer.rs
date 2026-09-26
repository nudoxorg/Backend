//! Restartable archive transfers owned by the content-addressed store.
//!
//! An incomplete transfer is a private `.part` file plus a checkpoint. A
//! completed transfer is linked into its digest path exactly once.

use super::{
    CHUNK_BYTES, ContentAddressedStore, ContentStoreError, ID_BYTES, MAX_VALIDATOR_BYTES,
    OBJECT_DOMAIN, ObjectAdmission, TEMP_TTL, digest_bytes, hex, now_millis, object_from_hash,
    read_lease_expiry, write_lease_marker,
};
use crate::acquisition::RawArchiveObjectId;
use blake3::Hasher;
use std::{
    fmt, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const TRANSFER_MAGIC: &[u8; 8] = b"NDOXTR02";

/// Stable identity for one resumable transfer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransferId([u8; ID_BYTES]);

impl TransferId {
    /// Derives an identity from adapter-neutral source and object fields.
    #[must_use]
    pub fn from_parts(
        source: &[u8],
        object: Option<RawArchiveObjectId>,
        extent: Option<u64>,
    ) -> Self {
        let object = object.map_or([0; ID_BYTES], RawArchiveObjectId::to_bytes);
        let extent = extent.unwrap_or(u64::MAX).to_be_bytes();
        Self(digest_bytes(&[source, &object, &extent]))
    }

    /// Returns the fixed-width transfer identity.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; ID_BYTES] {
        self.0
    }
}

/// HTTP representation validators persisted alongside an incomplete transfer.
///
/// The raw header values are retained because `If-Range` has deliberately
/// different rules for strong entity tags and HTTP dates.  The transport
/// chooses the strongest usable value when it constructs a request.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TransferValidator {
    etag: Option<Box<str>>,
    last_modified: Option<Box<str>>,
}

impl TransferValidator {
    /// Creates a validator from response header values, dropping empty values.
    #[must_use]
    pub fn new(etag: Option<&str>, last_modified: Option<&str>) -> Option<Self> {
        let etag = bounded_header(etag);
        let last_modified = bounded_http_date(last_modified);
        (etag.is_some() || last_modified.is_some()).then_some(Self {
            etag,
            last_modified,
        })
    }

    /// Returns the entity tag, if the origin supplied one.
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// Returns the last-modified date, if the origin supplied one.
    #[must_use]
    pub fn last_modified(&self) -> Option<&str> {
        self.last_modified.as_deref()
    }

    /// Returns a safe `If-Range` value. Weak entity tags cannot be used for
    /// range validation, so a last-modified date is preferred in that case.
    #[must_use]
    pub fn if_range(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|value| is_strong_etag(value))
            .or(self.last_modified())
    }
}

fn bounded_header(value: Option<&str>) -> Option<Box<str>> {
    let value = value?.trim();
    (!value.is_empty()
        && value.len() <= MAX_VALIDATOR_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii() && byte >= 0x20 && byte != 0x7f))
    .then(|| value.into())
}

fn is_strong_etag(value: &str) -> bool {
    value.len() >= 2 && !value.starts_with("W/") && value.starts_with('"') && value.ends_with('"')
}

fn bounded_http_date(value: Option<&str>) -> Option<Box<str>> {
    let value = bounded_header(value)?;
    let bytes = value.as_bytes();
    if bytes.len() != 29
        || bytes[3] != b','
        || bytes[4] != b' '
        || bytes[7] != b' '
        || bytes[11] != b' '
        || bytes[16] != b' '
        || bytes[19] != b':'
        || bytes[22] != b':'
        || bytes[25] != b' '
        || &bytes[26..] != b"GMT"
        || !bytes[..3].iter().all(u8::is_ascii_alphabetic)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || !bytes[8..11].iter().all(u8::is_ascii_alphabetic)
        || !bytes[12..16].iter().all(u8::is_ascii_digit)
        || !bytes[17..19].iter().all(u8::is_ascii_digit)
        || !bytes[20..22].iter().all(u8::is_ascii_digit)
        || !bytes[23..25].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    Some(value)
}

/// Why an in-flight response forces a transfer to restart at byte zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferResetReason {
    /// The origin supplied a different representation validator.
    ValidatorChanged,
    /// The origin ignored a requested range and returned a full body.
    RangeIgnored,
}

impl TransferResetReason {
    /// Stable edge label for telemetry, diagnostics, and quarantine names.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ValidatorChanged => "validator-change",
            Self::RangeIgnored => "range-ignored",
        }
    }

    const fn quarantine_prefix(self) -> bool {
        matches!(self, Self::ValidatorChanged)
    }
}

/// Byte accounting for one archive transfer attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransferTelemetry {
    /// Bytes already present locally when the transfer was opened.
    pub resumed_bytes: u64,
    /// Bytes read from the current upstream response.
    pub downloaded_bytes: u64,
}

/// Durable progress exposed to a range-capable transport adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferCheckpoint {
    /// Transfer identity.
    pub id: TransferId,
    /// Optional authenticated final object.
    pub expected: Option<RawArchiveObjectId>,
    /// Optional authenticated final extent.
    pub expected_length: Option<u64>,
    /// Bytes already durably appended.
    pub received: u64,
    /// Representation validator authenticated for the persisted prefix.
    pub validator: Option<TransferValidator>,
}

/// A durable append-only transfer that survives process restart.
pub struct ResumableTransfer {
    store: ContentAddressedStore,
    checkpoint: TransferCheckpoint,
    data_path: PathBuf,
    state_path: PathBuf,
    owner_path: PathBuf,
    owner_token: [u8; ID_BYTES],
    file: Option<File>,
    hasher: Option<Hasher>,
    telemetry: TransferTelemetry,
    finished: bool,
}

impl fmt::Debug for ResumableTransfer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResumableTransfer")
            .field("checkpoint", &self.checkpoint)
            .field("data_path", &self.data_path)
            .finish_non_exhaustive()
    }
}

impl ResumableTransfer {
    pub(super) fn open(
        store: ContentAddressedStore,
        id: TransferId,
        expected: Option<RawArchiveObjectId>,
        expected_length: Option<u64>,
    ) -> Result<Self, ContentStoreError> {
        let key = hex(&id.0);
        let data_path = store.root.join("transfers").join(format!("{key}.part"));
        let state_path = store.root.join("transfers").join(format!("{key}.state"));
        let owner_path = store.root.join("transfers").join(format!("{key}.owner"));
        let owner_token = store.next_token();
        acquire_transfer_owner(&owner_path, owner_token)?;

        let cleanup_owner_path = owner_path.clone();
        let open_result = (|| {
            let mut file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .append(true)
                .open(&data_path)?;
            let file_length = file.metadata()?.len();
            let saved_checkpoint = read_checkpoint(&state_path)?;
            let persisted_length = saved_checkpoint
                .as_ref()
                .and_then(|saved| saved.expected_length);
            if let Some(saved) = &saved_checkpoint {
                if saved.id != id
                    || saved.expected != expected
                    || expected_length.is_some_and(|length| saved.expected_length != Some(length))
                    || saved.received > file_length
                {
                    return Err(ContentStoreError::TransferStateMismatch);
                }
                // The checkpoint is the commit record for each append.  A
                // kill between durable data and durable checkpoint leaves an
                // uncommitted tail; discard only that tail and resume from
                // the last authenticated offset.
                if file_length > saved.received {
                    file.set_len(saved.received)?;
                    file.sync_data()?;
                }
            } else if file_length != 0 {
                // Bytes without a checkpoint were never authenticated by the
                // transfer protocol. Treat them as an interrupted first
                // write instead of admitting an orphaned prefix.
                file.set_len(0)?;
                file.sync_data()?;
            }
            let received = saved_checkpoint.as_ref().map_or(0, |saved| saved.received);
            let expected_length = expected_length.or(persisted_length);
            if expected_length.is_some_and(|length| received > length) {
                return Err(ContentStoreError::LengthMismatch {
                    expected: expected_length.unwrap_or(received),
                    actual: received,
                });
            }
            let mut hasher = Hasher::new();
            hasher.update(OBJECT_DOMAIN);
            file.seek(SeekFrom::Start(0))?;
            let mut buffer = [0_u8; CHUNK_BYTES];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            file.seek(SeekFrom::End(0))?;
            let checkpoint = TransferCheckpoint {
                id,
                expected,
                expected_length,
                received,
                validator: saved_checkpoint.and_then(|saved| saved.validator),
            };
            write_checkpoint(&state_path, &checkpoint)?;
            Ok(Self {
                store: store.clone(),
                checkpoint,
                data_path,
                state_path,
                owner_path,
                owner_token,
                file: Some(file),
                hasher: Some(hasher),
                telemetry: TransferTelemetry {
                    resumed_bytes: received,
                    downloaded_bytes: 0,
                },
                finished: false,
            })
        })();
        if open_result.is_err() {
            let _ = fs::remove_file(cleanup_owner_path);
        }
        open_result
    }

    /// Returns the range start an adapter should request next.
    #[must_use]
    pub const fn resume_offset(&self) -> u64 {
        self.checkpoint.received
    }

    /// Returns the latest durable checkpoint.
    #[must_use]
    pub fn checkpoint(&self) -> TransferCheckpoint {
        self.checkpoint.clone()
    }

    /// Returns the validator bound to the persisted prefix.
    #[must_use]
    pub fn validator(&self) -> Option<&TransferValidator> {
        self.checkpoint.validator.as_ref()
    }

    /// Returns byte accounting for this transfer handle.
    #[must_use]
    pub const fn telemetry(&self) -> TransferTelemetry {
        self.telemetry
    }

    /// Binds the representation validator before appending a response body.
    pub fn set_validator(
        &mut self,
        validator: Option<TransferValidator>,
    ) -> Result<(), ContentStoreError> {
        if self.checkpoint.received != 0
            && self.checkpoint.validator.is_some()
            && self.checkpoint.validator != validator
        {
            return Err(ContentStoreError::TransferValidatorChanged);
        }
        self.checkpoint.validator = validator;
        write_checkpoint(&self.state_path, &self.checkpoint)
    }

    /// Binds the authenticated final extent once a response exposes it.
    pub fn bind_expected_length(&mut self, length: u64) -> Result<(), ContentStoreError> {
        if let Some(expected) = self.checkpoint.expected_length
            && expected != length
        {
            return Err(ContentStoreError::LengthMismatch {
                expected,
                actual: length,
            });
        }
        if self.checkpoint.received > length {
            return Err(ContentStoreError::LengthMismatch {
                expected: length,
                actual: self.checkpoint.received,
            });
        }
        self.checkpoint.expected_length = Some(length);
        write_checkpoint(&self.state_path, &self.checkpoint)
    }

    /// Restarts the transfer at byte zero, preserving no unvalidated prefix.
    pub fn restart_from_zero(
        &mut self,
        reason: Option<TransferResetReason>,
    ) -> Result<(), ContentStoreError> {
        let had_prefix = self.checkpoint.received != 0;
        if had_prefix {
            // Publish the reset in the checkpoint before moving or truncating
            // bytes. A kill in either window then reopens as an empty transfer
            // and can safely discard any uncommitted old tail.
            self.checkpoint.received = 0;
            self.checkpoint.validator = None;
            if reason.is_some() {
                self.checkpoint.expected_length = None;
            }
            write_checkpoint(&self.state_path, &self.checkpoint)?;
            if let Some(reason) = reason
                && reason.quarantine_prefix()
            {
                self.quarantine_prefix(reason)?;
            }
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer is closed")))?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        self.checkpoint.received = 0;
        self.checkpoint.validator = None;
        if reason.is_some() {
            // A validator change means this is a different representation;
            // its final extent must be learned from the new response.
            self.checkpoint.expected_length = None;
        }
        self.telemetry.resumed_bytes = 0;
        self.hasher = Some({
            let mut hasher = Hasher::new();
            hasher.update(OBJECT_DOMAIN);
            hasher
        });
        write_checkpoint(&self.state_path, &self.checkpoint)
    }

    /// Renews the transfer lease while a remote range request is in flight.
    pub fn renew(&self) -> Result<(), ContentStoreError> {
        let expires = now_millis().saturating_add(TEMP_TTL.as_millis() as u64);
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.owner_path)?;
        file.write_all(&self.owner_token)?;
        file.write_all(&expires.to_be_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    /// Appends one bounded response body and persists its new range start.
    pub fn append<R: Read>(
        &mut self,
        mut source: R,
        maximum: u64,
    ) -> Result<u64, ContentStoreError> {
        let mut buffer = [0_u8; CHUNK_BYTES];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let next = self
                .checkpoint
                .received
                .checked_add(read as u64)
                .ok_or(ContentStoreError::Bounds { maximum })?;
            if next > maximum
                || self
                    .checkpoint
                    .expected_length
                    .is_some_and(|length| next > length)
            {
                return Err(ContentStoreError::Bounds { maximum });
            }
            let previous = self.checkpoint.received;
            let write_result = {
                let file = self
                    .file
                    .as_mut()
                    .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer is closed")))?;
                let result = file
                    .write_all(&buffer[..read])
                    .and_then(|()| file.flush())
                    .and_then(|()| file.sync_data());
                if result.is_err() {
                    let rollback = file
                        .set_len(previous)
                        .and_then(|()| file.seek(SeekFrom::End(0)).map(|_| ()));
                    if let Err(error) = rollback {
                        return Err(error.into());
                    }
                }
                result
            };
            if let Err(error) = write_result {
                return Err(error.into());
            }
            self.checkpoint.received = next;
            if let Err(error) = write_checkpoint(&self.state_path, &self.checkpoint) {
                self.checkpoint.received = previous;
                self.rollback_append(previous)?;
                return Err(error);
            }
            self.hasher
                .as_mut()
                .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer has no digest")))?
                .update(&buffer[..read]);
            self.telemetry.downloaded_bytes =
                self.telemetry.downloaded_bytes.saturating_add(read as u64);
        }
        Ok(self.checkpoint.received)
    }

    fn rollback_append(&mut self, previous: u64) -> Result<(), ContentStoreError> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer is closed")))?;
        file.set_len(previous)?;
        file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    fn quarantine_prefix(&mut self, reason: TransferResetReason) -> Result<(), ContentStoreError> {
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }
        let destination = self.store.root.join("quarantine").join(format!(
            "{}.{}.prefix",
            hex(&self.owner_token),
            reason.code()
        ));
        fs::rename(&self.data_path, destination)?;
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .append(true)
                .open(&self.data_path)?,
        );
        Ok(())
    }

    /// Verifies and atomically publishes the completed transfer.
    pub fn finish(self) -> Result<ObjectAdmission, ContentStoreError> {
        self.finish_verified(|_path, _bytes, _object| Ok(()))
    }

    /// Verifies a completed transfer with an adapter before publishing it.
    ///
    /// This is the durable counterpart to
    /// [`ContentAddressedStore::admit_reader_verified`]. The transfer bytes
    /// remain private until the verifier accepts them, so a process crash or
    /// a rejected checksum cannot leave a reachable untrusted object.
    pub fn finish_verified<V>(mut self, verifier: V) -> Result<ObjectAdmission, ContentStoreError>
    where
        V: FnOnce(&Path, u64, RawArchiveObjectId) -> Result<(), ContentStoreError>,
    {
        let received = self.checkpoint.received;
        if let Some(expected_length) = self.checkpoint.expected_length
            && received != expected_length
        {
            return Err(ContentStoreError::LengthMismatch {
                expected: expected_length,
                actual: received,
            });
        }
        let hash = *self
            .hasher
            .take()
            .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer has no digest")))?
            .finalize()
            .as_bytes();
        let actual = object_from_hash(received, hash);
        if let Some(expected) = self.checkpoint.expected
            && expected != actual
        {
            let quarantine = self.quarantine("digest-mismatch")?;
            return Err(ContentStoreError::DigestMismatch {
                expected,
                actual,
                quarantine: Some(quarantine),
            });
        }
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }
        if let Err(error) = verifier(&self.data_path, received, actual) {
            let quarantine = self.quarantine("verification").ok();
            if matches!(error, ContentStoreError::VerificationRejected { .. }) {
                return Err(ContentStoreError::VerificationRejected { quarantine });
            }
            return Err(error);
        }
        let target = self.store.object_path_for(actual);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        self.file.take();
        let result = match fs::hard_link(&self.data_path, &target) {
            Ok(()) => ObjectAdmission::Published {
                object: actual,
                bytes: received,
            },
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => ObjectAdmission::Reused {
                object: actual,
                bytes: self.store.verify_object(actual, received)?,
            },
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                let mut source = File::open(&self.data_path)?;
                let mut destination = match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        return self.finish_existing(actual);
                    }
                    Err(error) => return Err(error.into()),
                };
                io::copy(&mut source, &mut destination)?;
                destination.sync_all()?;
                ObjectAdmission::Published {
                    object: actual,
                    bytes: received,
                }
            }
            Err(error) => return Err(error.into()),
        };
        fs::remove_file(&self.data_path)?;
        let _ = fs::remove_file(&self.state_path);
        let _ = fs::remove_file(&self.owner_path);
        self.finished = true;
        Ok(result)
    }

    fn finish_existing(
        mut self,
        object: RawArchiveObjectId,
    ) -> Result<ObjectAdmission, ContentStoreError> {
        let bytes = self.store.verify_object(object, self.checkpoint.received)?;
        fs::remove_file(&self.data_path)?;
        let _ = fs::remove_file(&self.state_path);
        let _ = fs::remove_file(&self.owner_path);
        self.finished = true;
        Ok(ObjectAdmission::Reused { object, bytes })
    }

    fn quarantine(&mut self, reason: &str) -> Result<PathBuf, ContentStoreError> {
        self.file.take();
        let destination = self.store.root.join("quarantine").join(format!(
            "{}.{}.part",
            hex(&self.owner_token),
            reason
        ));
        fs::rename(&self.data_path, &destination)?;
        let _ = fs::remove_file(&self.state_path);
        let _ = fs::remove_file(&self.owner_path);
        self.finished = true;
        Ok(destination)
    }
}

impl Drop for ResumableTransfer {
    fn drop(&mut self) {
        let _ = self.file.take();
        if !self.finished {
            let _ = fs::remove_file(&self.owner_path);
        }
    }
}

fn acquire_transfer_owner(path: &Path, token: [u8; ID_BYTES]) -> Result<(), ContentStoreError> {
    let expires = now_millis().saturating_add(TEMP_TTL.as_millis() as u64);
    match write_lease_marker(path, token, expires) {
        Ok(()) => Ok(()),
        Err(ContentStoreError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
            if read_lease_expiry(path).is_some_and(|expiry| expiry <= now_millis()) {
                let _ = fs::remove_file(path);
                write_lease_marker(
                    path,
                    token,
                    now_millis().saturating_add(TEMP_TTL.as_millis() as u64),
                )
            } else {
                Err(ContentStoreError::TransferBusy)
            }
        }
        Err(error) => Err(error),
    }
}

fn write_checkpoint(path: &Path, checkpoint: &TransferCheckpoint) -> Result<(), ContentStoreError> {
    let mut bytes = Vec::with_capacity(8 + ID_BYTES + 1 + ID_BYTES + 8 + 8 + 2 + 2 + 16_384);
    bytes.extend_from_slice(TRANSFER_MAGIC);
    bytes.extend_from_slice(&checkpoint.id.0);
    match checkpoint.expected {
        Some(object) => {
            bytes.push(1);
            bytes.extend_from_slice(object.as_bytes());
        }
        None => {
            bytes.push(0);
            bytes.extend_from_slice(&[0; ID_BYTES]);
        }
    }
    bytes.extend_from_slice(&checkpoint.expected_length.unwrap_or(u64::MAX).to_be_bytes());
    bytes.extend_from_slice(&checkpoint.received.to_be_bytes());
    for value in [
        checkpoint
            .validator
            .as_ref()
            .and_then(|validator| validator.etag()),
        checkpoint
            .validator
            .as_ref()
            .and_then(|validator| validator.last_modified()),
    ] {
        let value = value.unwrap_or_default().as_bytes();
        let length =
            u16::try_from(value.len()).map_err(|_| ContentStoreError::TransferStateMismatch)?;
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(value);
    }
    let temporary = path.with_extension("state.tmp");
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_checkpoint(path: &Path) -> Result<Option<TransferCheckpoint>, ContentStoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    const PREFIX: usize = 8 + ID_BYTES + 1 + ID_BYTES + 8 + 8;
    if bytes.len() < PREFIX || &bytes[..8] != TRANSFER_MAGIC {
        return Err(ContentStoreError::TransferStateMismatch);
    }
    let mut id = [0; ID_BYTES];
    id.copy_from_slice(&bytes[8..8 + ID_BYTES]);
    let marker = bytes[8 + ID_BYTES];
    if marker > 1 {
        return Err(ContentStoreError::TransferStateMismatch);
    }
    let mut object_bytes = [0; ID_BYTES];
    object_bytes.copy_from_slice(&bytes[9 + ID_BYTES..9 + ID_BYTES * 2]);
    let mut extent = [0; 8];
    extent.copy_from_slice(&bytes[9 + ID_BYTES * 2..9 + ID_BYTES * 2 + 8]);
    let mut received = [0; 8];
    received.copy_from_slice(&bytes[PREFIX - 8..PREFIX]);
    let mut at = PREFIX;
    let read_value =
        |bytes: &[u8], at: &mut usize| -> Result<Option<Box<str>>, ContentStoreError> {
            if bytes.len().saturating_sub(*at) < 2 {
                return Err(ContentStoreError::TransferStateMismatch);
            }
            let mut length = [0_u8; 2];
            length.copy_from_slice(&bytes[*at..*at + 2]);
            *at += 2;
            let length = usize::from(u16::from_be_bytes(length));
            let end = (*at)
                .checked_add(length)
                .ok_or(ContentStoreError::TransferStateMismatch)?;
            if end > bytes.len() {
                return Err(ContentStoreError::TransferStateMismatch);
            }
            let value = std::str::from_utf8(&bytes[*at..end])
                .map_err(|_| ContentStoreError::TransferStateMismatch)?;
            *at = end;
            Ok((!value.is_empty()).then(|| value.into()))
        };
    let etag = read_value(&bytes, &mut at)?;
    let last_modified = read_value(&bytes, &mut at)?;
    if at != bytes.len() {
        return Err(ContentStoreError::TransferStateMismatch);
    }
    let validator = TransferValidator::new(etag.as_deref(), last_modified.as_deref());
    if etag.is_some_and(|_| {
        validator
            .as_ref()
            .and_then(TransferValidator::etag)
            .is_none()
    }) || last_modified.is_some_and(|_| {
        validator
            .as_ref()
            .and_then(TransferValidator::last_modified)
            .is_none()
    }) {
        return Err(ContentStoreError::TransferStateMismatch);
    }
    Ok(Some(TransferCheckpoint {
        id: TransferId(id),
        expected: (marker == 1).then(|| RawArchiveObjectId::from_encoded(object_bytes)),
        expected_length: (u64::from_be_bytes(extent) != u64::MAX)
            .then_some(u64::from_be_bytes(extent)),
        received: u64::from_be_bytes(received),
        validator,
    }))
}
