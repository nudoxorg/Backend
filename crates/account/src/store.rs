//! Where the key lives: `NUDOX_API_KEY` when it is set, otherwise the
//! owner-only `api-key` file in the account directory.
//!
//! The same directory keeps two small non-secret files for the MCP meter:
//! when the key was last confirmed by the service, and how many tool calls
//! ran while the service could not be reached and still need recording.

use crate::key::{ApiKey, KeyError};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Environment variable that supplies the key instead of the saved file.
pub const API_KEY_ENV: &str = "NUDOX_API_KEY";

const KEY_FILE: &str = "api-key";
const VERIFIED_FILE: &str = "verified-at";
const PENDING_FILE: &str = "pending-tool-calls";
/// Large enough for a key and a newline; anything bigger is not ours.
const MOST_FILE_BYTES: u64 = 256;

/// Where a loaded key came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeySource {
    /// The saved key file.
    File(PathBuf),
    /// The [`API_KEY_ENV`] environment variable.
    Environment,
}

impl KeySource {
    /// How a settings page or `nudox auth status` names it.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::File(path) => format!("saved in {}", path.display()),
            Self::Environment => format!("from {API_KEY_ENV}"),
        }
    }
}

/// Why the key could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The per-user account directory could not be found or made private.
    #[error("cannot open the Nudox account directory: {0}")]
    Directory(String),
    /// [`API_KEY_ENV`] is set to something that is not a key.
    #[error("{API_KEY_ENV} is set but is not a Nudox API key: {0}")]
    Environment(KeyError),
    /// The saved file is not a key; signing in again replaces it.
    #[error("the saved API key at {} is damaged ({reason}); sign in again to replace it", path.display())]
    Damaged {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// The file system refused the read or write.
    #[error("cannot {action} the saved API key at {}: {source}", path.display())]
    Io {
        /// `read`, `save` or `remove`.
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

/// The account directory and the files in it.
#[derive(Clone, Debug)]
pub struct KeyStore {
    directory: PathBuf,
}

impl KeyStore {
    /// Opens the per-user account directory shared by every Nudox surface,
    /// creating it owner-only on first use.
    ///
    /// # Errors
    /// Returns [`StoreError::Directory`] when the user's state directory is
    /// unavailable or not private.
    pub fn open() -> Result<Self, StoreError> {
        backend_runtime::account_directory()
            .map(Self::at)
            .map_err(|error| StoreError::Directory(error.to_string()))
    }

    /// Uses an existing private directory, for tests.
    #[must_use]
    pub fn at(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// The key every surface should use: the environment first, then the file.
    ///
    /// # Errors
    /// Returns an error when the environment value or the saved file exists
    /// but is not a usable key.
    pub fn load(&self) -> Result<Option<(ApiKey, KeySource)>, StoreError> {
        self.load_with(std::env::var(API_KEY_ENV).ok())
    }

    fn load_with(
        &self,
        environment: Option<String>,
    ) -> Result<Option<(ApiKey, KeySource)>, StoreError> {
        if let Some(value) = environment.filter(|value| !value.trim().is_empty()) {
            let key = ApiKey::parse(&value).map_err(StoreError::Environment)?;
            return Ok(Some((key, KeySource::Environment)));
        }
        let path = self.path(KEY_FILE);
        let Some(text) = read_small(&path).map_err(|source| StoreError::Io {
            action: "read",
            path: path.clone(),
            source,
        })?
        else {
            return Ok(None);
        };
        let key = ApiKey::parse(&text).map_err(|error| StoreError::Damaged {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        Ok(Some((key, KeySource::File(path))))
    }

    /// Saves a key the service has just accepted, and records that acceptance.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] when the file cannot be written privately.
    pub fn save(&self, key: &ApiKey) -> Result<(), StoreError> {
        let path = self.path(KEY_FILE);
        backend_platform::durable::write_private_atomic(&path, key.expose().as_bytes()).map_err(
            |source| StoreError::Io {
                action: "save",
                path,
                source,
            },
        )?;
        self.clear_pending();
        self.mark_verified(SystemTime::now());
        Ok(())
    }

    /// Signs out: removes the saved key and the meter's state.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] when the file exists but cannot be removed.
    pub fn remove(&self) -> Result<(), StoreError> {
        for name in [VERIFIED_FILE, PENDING_FILE] {
            let _ = remove_if_present(&self.path(name));
        }
        let path = self.path(KEY_FILE);
        remove_if_present(&path).map_err(|source| StoreError::Io {
            action: "remove",
            path,
            source,
        })
    }

    /// When the service last confirmed the key.
    #[must_use]
    pub fn verified_at(&self) -> Option<SystemTime> {
        let text = read_small(&self.path(VERIFIED_FILE)).ok()??;
        let seconds = text.trim().parse::<u64>().ok()?;
        UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
    }

    /// Records a confirmation. A lost write only shortens the offline grace.
    pub fn mark_verified(&self, at: SystemTime) {
        let seconds = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let _ = backend_platform::durable::write_private_atomic_unflushed(
            &self.path(VERIFIED_FILE),
            seconds.to_string().as_bytes(),
        );
    }

    /// Tool calls that ran offline and have not been recorded yet.
    #[must_use]
    pub fn pending(&self) -> i64 {
        read_small(&self.path(PENDING_FILE))
            .ok()
            .flatten()
            .and_then(|text| text.trim().parse::<i64>().ok())
            .filter(|count| *count > 0)
            .unwrap_or(0)
    }

    /// Replaces the offline tool-call count.
    pub fn set_pending(&self, count: i64) {
        let _ = backend_platform::durable::write_private_atomic(
            &self.path(PENDING_FILE),
            count.to_string().as_bytes(),
        );
    }

    /// Forgets offline tool calls once they are recorded (or dropped).
    pub fn clear_pending(&self) {
        let _ = remove_if_present(&self.path(PENDING_FILE));
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
}

fn read_small(path: &Path) -> io::Result<Option<String>> {
    let file = match backend_platform::durable::open_private_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut text = String::new();
    file.take(MOST_FILE_BYTES).read_to_string(&mut text)?;
    Ok(Some(text))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match backend_platform::durable::remove_private(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const KEY: &str =
        "ndx_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    pub(crate) fn private_store() -> (tempfile::TempDir, KeyStore) {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("account");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
                .expect("private temp root");
        }
        backend_platform::durable::ensure_private_directory(&directory).expect("private dir");
        (root, KeyStore::at(directory))
    }

    #[test]
    fn a_saved_key_round_trips_and_sign_out_removes_it() {
        let (_root, store) = private_store();
        assert!(store.load_with(None).expect("load").is_none());
        let key = ApiKey::parse(KEY).expect("key");
        store.save(&key).expect("save");
        let (loaded, source) = store.load_with(None).expect("load").expect("saved");
        assert_eq!(loaded, key);
        assert!(matches!(source, KeySource::File(_)));
        assert!(store.verified_at().is_some());
        store.remove().expect("remove");
        assert!(store.load_with(None).expect("load").is_none());
        assert!(store.verified_at().is_none());
        store.remove().expect("removing twice is fine");
    }

    #[test]
    fn the_environment_wins_and_must_be_a_key() {
        let (_root, store) = private_store();
        let (_, source) = store
            .load_with(Some(KEY.to_owned()))
            .expect("load")
            .expect("from env");
        assert_eq!(source, KeySource::Environment);
        assert!(matches!(
            store.load_with(Some("nope".to_owned())),
            Err(StoreError::Environment(KeyError::MissingPrefix))
        ));
        assert!(
            store
                .load_with(Some("  ".to_owned()))
                .expect("blank")
                .is_none()
        );
    }

    #[test]
    fn a_damaged_file_says_to_sign_in_again() {
        let (_root, store) = private_store();
        backend_platform::durable::write_private_atomic(&store.path(KEY_FILE), b"ndx_short")
            .expect("write");
        let error = store.load_with(None).expect_err("damaged");
        assert!(error.to_string().contains("sign in again"), "{error}");
    }

    #[test]
    fn pending_counts_survive_and_clear() {
        let (_root, store) = private_store();
        assert_eq!(store.pending(), 0);
        store.set_pending(3);
        assert_eq!(store.pending(), 3);
        store.clear_pending();
        assert_eq!(store.pending(), 0);
    }
}
