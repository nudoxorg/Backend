//! Per-user, per-machine memo of facts that can always be recomputed.
//!
//! A record here is never authority. Every caller validates a record against
//! a fresh, cheap observation of the thing it describes and recomputes the fact
//! when the observation differs, so a missing, stale, truncated, or unwritable
//! cache only costs time. Writes are therefore atomic (a private sibling plus a
//! rename) but deliberately not flushed: a lost record is a cache miss, and a
//! flush on a busy journal would defeat the purpose of caching startup work.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Largest key or value a record may carry.
const MAX_FIELD_BYTES: usize = 4096;
const MAGIC: &[u8; 8] = b"NXMEMO01";
const APPLICATION_DIRECTORY: &str = "Nudox";
const MEMO_DIRECTORY: &str = "memo";

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Selects the operating system's per-user cache directory for this
/// application without creating it.
///
/// Returns `None` when the environment names no absolute cache location; the
/// cache is then simply unavailable.
#[must_use]
pub fn user_cache_directory() -> Option<PathBuf> {
    cache_root().map(|root| root.join(APPLICATION_DIRECTORY))
}

#[cfg(target_os = "macos")]
fn cache_root() -> Option<PathBuf> {
    absolute_env("HOME").map(|home| home.join("Library").join("Caches"))
}

#[cfg(windows)]
fn cache_root() -> Option<PathBuf> {
    absolute_env("LOCALAPPDATA")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn cache_root() -> Option<PathBuf> {
    absolute_env("XDG_CACHE_HOME").or_else(|| absolute_env("HOME").map(|home| home.join(".cache")))
}

#[cfg(not(any(unix, windows)))]
fn cache_root() -> Option<PathBuf> {
    None
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    path.is_absolute().then_some(path)
}

/// One namespace of recomputable key/value records beneath a cache directory.
#[derive(Clone, Debug)]
pub struct DerivedFactMemo {
    directory: PathBuf,
}

impl DerivedFactMemo {
    /// Selects `namespace` beneath the user's cache directory without
    /// touching the filesystem. `None` means the platform names no cache.
    #[must_use]
    pub fn in_user_cache(namespace: &str) -> Option<Self> {
        Some(Self::beneath(&user_cache_directory()?, namespace))
    }

    /// Selects `namespace` beneath an explicit cache directory.
    #[must_use]
    pub fn beneath(cache: &Path, namespace: &str) -> Self {
        Self {
            directory: cache.join(MEMO_DIRECTORY).join(namespace),
        }
    }

    /// Returns the value last stored for exactly `key`, if a complete record
    /// for it is present. Any read or format problem is a miss.
    #[must_use]
    pub fn load(&self, key: &[u8]) -> Option<Vec<u8>> {
        if key.len() > MAX_FIELD_BYTES {
            return None;
        }
        let mut file = File::open(self.record_path(key)).ok()?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(u64::try_from(MAGIC.len() + 8 + 2 * MAX_FIELD_BYTES).unwrap_or(u64::MAX) + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        decode(&bytes, key)
    }

    /// Replaces the record for `key`. Best effort: the caller keeps working
    /// from the value it computed when this fails.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error that prevented publication.
    pub fn store(&self, key: &[u8], value: &[u8]) -> io::Result<()> {
        if key.len() > MAX_FIELD_BYTES || value.len() > MAX_FIELD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "memo record field exceeds its bound",
            ));
        }
        create_private_directories(&self.directory)?;
        let destination = self.record_path(key);
        let temporary = self.directory.join(format!(
            ".{}.{}.{}.tmp",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            record_name(key)
        ));
        let result = (|| {
            let mut file = create_private_file(&temporary)?;
            file.write_all(&encode(key, value))?;
            drop(file);
            fs::rename(&temporary, &destination)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn record_path(&self, key: &[u8]) -> PathBuf {
        self.directory.join(record_name(key))
    }
}

/// A stable, non-cryptographic file name. Collisions are harmless because a
/// record carries and is checked against its complete key.
fn record_name(key: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in key {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}-{:04x}.memo", key.len() & 0xffff)
}

fn encode(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAGIC.len() + 8 + key.len() + value.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&u32::try_from(key.len()).unwrap_or(u32::MAX).to_be_bytes());
    bytes.extend_from_slice(key);
    bytes.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
    bytes.extend_from_slice(value);
    bytes
}

fn decode(bytes: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let (stored_length, rest) = split_length(rest)?;
    if stored_length != key.len() || rest.len() < stored_length {
        return None;
    }
    let (stored_key, rest) = rest.split_at(stored_length);
    if stored_key != key {
        return None;
    }
    let (value_length, rest) = split_length(rest)?;
    // A record is complete only when it ends exactly after its value.
    (value_length <= MAX_FIELD_BYTES && rest.len() == value_length).then(|| rest.to_vec())
}

fn split_length(bytes: &[u8]) -> Option<(usize, &[u8])> {
    let (head, rest) = bytes.split_at_checked(4)?;
    let length = u32::from_be_bytes(head.try_into().ok()?);
    Some((usize::try_from(length).ok()?, rest))
}

#[cfg(unix)]
fn create_private_directories(directory: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
}

#[cfg(not(unix))]
fn create_private_directories(directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "backend-platform-memo-{label}-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn a_stored_record_is_returned_only_for_its_exact_key() {
        let root = scratch("roundtrip");
        let memo = DerivedFactMemo::beneath(&root, "unit");
        assert_eq!(memo.load(b"alpha"), None);
        memo.store(b"alpha", b"one").expect("store");
        assert_eq!(memo.load(b"alpha").as_deref(), Some(&b"one"[..]));
        assert_eq!(memo.load(b"alphb"), None);
        memo.store(b"alpha", b"two").expect("replace");
        assert_eq!(memo.load(b"alpha").as_deref(), Some(&b"two"[..]));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_truncated_or_foreign_record_is_a_miss() {
        let root = scratch("damaged");
        let memo = DerivedFactMemo::beneath(&root, "unit");
        memo.store(b"key", b"value").expect("store");
        let path = memo.record_path(b"key");
        let full = fs::read(&path).expect("record");
        for cut in 0..full.len() {
            fs::write(&path, &full[..cut]).expect("truncate");
            assert_eq!(memo.load(b"key"), None, "cut at {cut}");
        }
        let mut trailing = full.clone();
        trailing.push(0);
        fs::write(&path, trailing).expect("trailing byte");
        assert_eq!(memo.load(b"key"), None);
        fs::write(&path, b"not a record").expect("foreign");
        assert_eq!(memo.load(b"key"), None);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn records_and_their_directories_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = scratch("private");
        let memo = DerivedFactMemo::beneath(&root, "unit");
        memo.store(b"key", b"value").expect("store");
        let file = fs::metadata(memo.record_path(b"key")).expect("record metadata");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let directory = fs::metadata(&memo.directory).expect("directory metadata");
        assert_eq!(directory.permissions().mode() & 0o077, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_unwritable_cache_reports_an_error_and_loads_as_a_miss() {
        let root = scratch("unwritable");
        fs::create_dir_all(&root).expect("root");
        let blocker = root.join("blocker");
        fs::write(&blocker, b"a file, not a directory").expect("blocker");
        let memo = DerivedFactMemo::beneath(&blocker, "unit");
        assert!(memo.store(b"key", b"value").is_err());
        assert_eq!(memo.load(b"key"), None);
        let _ = fs::remove_dir_all(root);
    }
}
