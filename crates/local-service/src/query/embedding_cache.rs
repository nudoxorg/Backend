//! Bounded durable cache for exact document-embedding inputs.
//!
//! Each input identity has one checksummed file, so a changed view writes only
//! its new embeddings. Entries are keyed by `EmbeddingInputIdentity`; that key
//! binds the exact model, tokenizer, executable, runtime options, treatment,
//! and semantic text. Recipe metadata remains in the file for diagnostics but
//! does not partition the cache: exact identities can survive a recipe switch.
//! Cache failures are misses because the cache is an optimization.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"BEMBC001";
const VERSION: u8 = 1;
const HEADER_BYTES: usize = 8 + 1 + 32 + 32 + 4;
const CHECKSUM_BYTES: usize = 32;
const CACHE_PARENT: &str = "embedding";
const CACHE_DOMAIN: &str = "backend.local-service.embedding-cache.v1";
const MAX_CACHE_BYTES: u64 = 520 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 65_536;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// One workspace-local directory of exact input-keyed vector entries.
pub(super) struct EmbeddingCacheFile {
    directory: PathBuf,
    recipe: [u8; 32],
    dimensions: u32,
    bytes_used: u64,
    entries: BTreeMap<[u8; 32], CacheEntry>,
    recency: u128,
}

#[derive(Clone, Copy)]
struct CacheEntry {
    bytes: u64,
    last_used: u128,
}

impl EmbeddingCacheFile {
    /// Opens the cache below the configured durable workspace directory.
    ///
    /// Absence of a durable workspace directory disables persistence while
    /// leaving the caller's bounded process-local cache available.
    pub(super) fn open(recipe: [u8; 32], dimensions: u32) -> Option<Self> {
        let root = std::env::var_os(backend_runtime::DATA_ENV).map(PathBuf::from)?;
        if !root.is_absolute() || dimensions == 0 {
            return None;
        }
        let directory = root.join("cache").join(CACHE_PARENT);
        create_private_directory(&directory).ok()?;
        migrate_legacy_recipe_directories(&directory).ok()?;
        Self::open_in_directory(&directory, recipe, dimensions)
    }

    pub(super) fn open_in_directory(
        directory: &Path,
        recipe: [u8; 32],
        dimensions: u32,
    ) -> Option<Self> {
        if !directory.is_absolute() || dimensions == 0 {
            return None;
        }
        create_private_directory(directory).ok()?;
        let directory = directory.to_path_buf();
        let (bytes_used, entries, recency) = match inventory(&directory) {
            Ok(inventory) => inventory,
            Err(()) => {
                fs::remove_dir_all(&directory).ok()?;
                create_private_directory(&directory).ok()?;
                (0, BTreeMap::new(), 0)
            }
        };
        if bytes_used > MAX_CACHE_BYTES || entries.len() > MAX_CACHE_ENTRIES {
            fs::remove_dir_all(&directory).ok()?;
            create_private_directory(&directory).ok()?;
            return Some(Self {
                directory,
                recipe,
                dimensions,
                bytes_used: 0,
                entries: BTreeMap::new(),
                recency: 0,
            });
        }
        Some(Self {
            directory,
            recipe,
            dimensions,
            bytes_used,
            entries,
            recency,
        })
    }

    /// Reads and validates one exact cache key. Corrupt entries become misses.
    pub(super) fn load(&mut self, identity: [u8; 32]) -> Option<Arc<[f32]>> {
        let path = self.entry_path(&identity);
        match self.load_checked(&path, identity) {
            Ok(values) => {
                self.touch(identity);
                Some(values)
            }
            Err(_) => {
                self.remove_entry(identity, &path);
                let _ = fs::remove_file(path);
                None
            }
        }
    }

    /// Stores newly computed entries, evicting least-recently-used entries
    /// only when the byte or entry cap requires it.
    pub(super) fn store_batch(&mut self, values: &[([u8; 32], &[f32])]) -> io::Result<()> {
        let dimension_count = usize::try_from(self.dimensions).map_err(|_| cache_size_error())?;
        let entry_bytes = u64::try_from(
            HEADER_BYTES
                .checked_add(
                    dimension_count
                        .checked_mul(4)
                        .ok_or_else(cache_size_error)?,
                )
                .and_then(|bytes| bytes.checked_add(CHECKSUM_BYTES))
                .ok_or_else(cache_size_error)?,
        )
        .map_err(|_| cache_size_error())?;
        let mut unique = BTreeMap::new();
        for (identity, coordinates) in values {
            if coordinates.len() != dimension_count
                || coordinates.iter().any(|value| !value.is_finite())
                || !is_unit_vector(coordinates)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid cached vector",
                ));
            }
            unique.insert(*identity, *coordinates);
        }
        let new_entries = unique
            .keys()
            .filter(|identity| !self.entries.contains_key(*identity))
            .count();
        let additional_bytes = entry_bytes
            .checked_mul(u64::try_from(new_entries).map_err(|_| cache_size_error())?)
            .ok_or_else(cache_size_error)?;
        let mut projected_bytes = self
            .bytes_used
            .checked_add(additional_bytes)
            .ok_or_else(cache_size_error)?;
        let mut projected_entries = self.entries.len().saturating_add(new_entries);
        if unique.len() > MAX_CACHE_ENTRIES || additional_bytes > MAX_CACHE_BYTES {
            return Ok(());
        }

        let mut eviction = self
            .entries
            .iter()
            .filter(|(identity, _)| !unique.contains_key(*identity))
            .map(|(identity, entry)| (*identity, entry.last_used))
            .collect::<Vec<_>>();
        eviction.sort_by_key(|(_, last_used)| *last_used);
        for (identity, _) in eviction {
            if projected_bytes <= MAX_CACHE_BYTES && projected_entries <= MAX_CACHE_ENTRIES {
                break;
            }
            let Some(entry) = self.entries.remove(&identity) else {
                continue;
            };
            let path = self.entry_path(&identity);
            if fs::remove_file(path).is_ok() {
                self.bytes_used = self.bytes_used.saturating_sub(entry.bytes);
            } else {
                self.entries.insert(identity, entry);
                continue;
            }
            projected_bytes = projected_bytes.saturating_sub(entry.bytes);
            projected_entries = projected_entries.saturating_sub(1);
        }
        if projected_bytes > MAX_CACHE_BYTES || projected_entries > MAX_CACHE_ENTRIES {
            return Ok(());
        }

        for (identity, coordinates) in unique {
            self.store_one(identity, coordinates, entry_bytes)?;
        }
        Ok(())
    }

    fn store_one(
        &mut self,
        identity: [u8; 32],
        values: &[f32],
        entry_bytes: u64,
    ) -> io::Result<()> {
        let path = self.entry_path(&identity);
        let prior = self.entries.get(&identity).copied();
        let temporary = self.temporary_path(&identity);
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            set_private_file_mode(&mut options);
            let mut file = options.open(&temporary)?;
            let mut hasher = blake3::Hasher::new_derive_key(CACHE_DOMAIN);
            let mut header = [0_u8; HEADER_BYTES];
            header[..8].copy_from_slice(MAGIC);
            header[8] = VERSION;
            header[9..41].copy_from_slice(&self.recipe);
            header[41..73].copy_from_slice(&identity);
            header[73..77].copy_from_slice(&self.dimensions.to_be_bytes());
            write_hashed(&mut file, &mut hasher, &header)?;
            for value in values {
                write_hashed(&mut file, &mut hasher, &value.to_le_bytes())?;
            }
            file.write_all(hasher.finalize().as_bytes())?;
            file.sync_all()?;
            replace_file(&temporary, &path)?;
            sync_parent(&path);
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
            return result;
        }
        let last_used = self.tick();
        self.bytes_used = self
            .bytes_used
            .saturating_sub(prior.map_or(0, |entry| entry.bytes))
            .saturating_add(entry_bytes);
        self.entries.insert(
            identity,
            CacheEntry {
                bytes: entry_bytes,
                last_used,
            },
        );
        Ok(())
    }

    fn touch(&mut self, identity: [u8; 32]) {
        let last_used = self.tick();
        if let Some(entry) = self.entries.get_mut(&identity) {
            entry.last_used = last_used;
        }
        // Reading the checksummed file naturally advances filesystem access
        // time where supported. Avoid a second write-open for every hit.
    }

    fn tick(&mut self) -> u128 {
        let now = timestamp(std::time::SystemTime::now());
        self.recency = self.recency.max(now).saturating_add(1);
        self.recency
    }

    fn remove_entry(&mut self, identity: [u8; 32], _path: &Path) {
        if let Some(entry) = self.entries.remove(&identity) {
            self.bytes_used = self.bytes_used.saturating_sub(entry.bytes);
        }
    }

    fn load_checked(&self, path: &Path, identity: [u8; 32]) -> io::Result<Arc<[f32]>> {
        let mut file = File::open(path)?;
        let dimensions = usize::try_from(self.dimensions).map_err(|_| cache_data_error())?;
        let expected = HEADER_BYTES
            .checked_add(dimensions.checked_mul(4).ok_or_else(cache_data_error)?)
            .and_then(|bytes| bytes.checked_add(CHECKSUM_BYTES))
            .ok_or_else(cache_data_error)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != u64::try_from(expected).unwrap_or(u64::MAX) {
            return Err(cache_data_error());
        }
        let mut hasher = blake3::Hasher::new_derive_key(CACHE_DOMAIN);
        let mut header = [0_u8; HEADER_BYTES];
        read_hashed(&mut file, &mut hasher, &mut header)?;
        if &header[..8] != MAGIC
            || header[8] != VERSION
            || header[41..73] != identity
            || header[73..77] != self.dimensions.to_be_bytes()
        {
            return Err(cache_data_error());
        }

        let mut values = Vec::new();
        values
            .try_reserve_exact(dimensions)
            .map_err(|_| io::Error::other("embedding cache allocation failed"))?;
        let mut coordinate = [0_u8; 4];
        let mut norm_squared = 0.0_f64;
        for _ in 0..dimensions {
            read_hashed(&mut file, &mut hasher, &mut coordinate)?;
            let value = f32::from_le_bytes(coordinate);
            if !value.is_finite() {
                return Err(cache_data_error());
            }
            norm_squared += f64::from(value) * f64::from(value);
            values.push(value);
        }
        if (norm_squared - 1.0).abs() > 0.001 {
            return Err(cache_data_error());
        }
        let expected_checksum = hasher.finalize();
        let mut observed_checksum = [0_u8; CHECKSUM_BYTES];
        file.read_exact(&mut observed_checksum)?;
        if observed_checksum != *expected_checksum.as_bytes() {
            return Err(cache_data_error());
        }
        Ok(Arc::from(values))
    }

    fn entry_path(&self, identity: &[u8; 32]) -> PathBuf {
        self.directory
            .join(format!("{}.vec", hexadecimal(identity)))
    }

    fn temporary_path(&self, identity: &[u8; 32]) -> PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.directory.join(format!(
            ".{}-{}-{sequence}.tmp",
            hexadecimal(identity),
            std::process::id()
        ))
    }
}

fn inventory(directory: &Path) -> Result<(u64, BTreeMap<[u8; 32], CacheEntry>, u128), ()> {
    let entries = fs::read_dir(directory).map_err(|_| ())?;
    let mut bytes = 0_u64;
    let mut inventory = BTreeMap::new();
    let mut recency = 0_u128;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_CACHE_ENTRIES.saturating_add(1) {
            return Err(());
        }
        let entry = entry.map_err(|_| ())?;
        let metadata = entry.file_type().map_err(|_| ())?;
        if !metadata.is_file() {
            return Err(());
        }
        let name = entry.file_name();
        let Some(identity) = name.to_str().and_then(parse_identity_filename) else {
            if name
                .to_str()
                .is_some_and(|name| name.starts_with('.') && name.ends_with(".tmp"))
            {
                let _ = fs::remove_file(entry.path());
                continue;
            }
            let _ = fs::remove_file(entry.path());
            continue;
        };
        let expected_name = format!("{}.vec", hexadecimal(&identity));
        if name.to_str() != Some(expected_name.as_str()) {
            let _ = fs::remove_file(entry.path());
            continue;
        }
        let metadata = entry.metadata().map_err(|_| ())?;
        let size = metadata.len();
        let last_used = metadata
            .accessed()
            .ok()
            .or_else(|| metadata.modified().ok())
            .map(timestamp)
            .unwrap_or_default();
        if inventory
            .insert(
                identity,
                CacheEntry {
                    bytes: size,
                    last_used,
                },
            )
            .is_some()
        {
            return Err(());
        }
        bytes = bytes.checked_add(size).ok_or(())?;
        recency = recency.max(last_used);
    }
    if inventory.len() > MAX_CACHE_ENTRIES {
        return Err(());
    }
    Ok((bytes, inventory, recency))
}

fn migrate_legacy_recipe_directories(directory: &Path) -> io::Result<()> {
    let entries = fs::read_dir(directory)?;
    for (index, entry) in entries.enumerate() {
        // The previous cache layout allowed only one recipe directory. Keep
        // migration work bounded if the cache path was populated externally.
        if index >= MAX_CACHE_ENTRIES.saturating_add(64) {
            break;
        }
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(recipe) = entry.file_name().to_str().and_then(parse_hexadecimal_32) else {
            // This namespace is reserved for embedding cache files. A
            // non-cache subdirectory cannot be allowed to make inventory
            // discard otherwise-valid entries from the global cache.
            fs::remove_dir_all(entry.path())?;
            continue;
        };
        let legacy = entry.path();
        let files = fs::read_dir(&legacy)?;
        for (file_index, file) in files.enumerate() {
            if file_index >= MAX_CACHE_ENTRIES {
                break;
            }
            let file = file?;
            if !file.file_type()?.is_file() {
                continue;
            }
            let name = file.file_name();
            let Some(identity) = name.to_str().and_then(parse_identity_filename) else {
                if name
                    .to_str()
                    .is_some_and(|name| name.starts_with('.') && name.ends_with(".tmp"))
                {
                    let _ = fs::remove_file(file.path());
                }
                continue;
            };
            let expected_name = format!("{}.vec", hexadecimal(&identity));
            if name.to_str() != Some(expected_name.as_str()) {
                continue;
            }
            let mut cache_file = File::open(file.path())?;
            let mut header = [0_u8; HEADER_BYTES];
            if cache_file.read_exact(&mut header).is_err()
                || &header[..8] != MAGIC
                || header[8] != VERSION
                || header[9..41] != recipe
                || header[41..73] != identity
            {
                let _ = fs::remove_file(file.path());
                continue;
            }
            let destination = directory.join(format!("{}.vec", hexadecimal(&identity)));
            if destination.exists() {
                let _ = fs::remove_file(file.path());
            } else if fs::rename(file.path(), destination).is_err() {
                return Err(io::Error::other("embedding cache migration failed"));
            }
        }
        fs::remove_dir_all(legacy)?;
    }
    Ok(())
}

fn parse_identity_filename(name: &str) -> Option<[u8; 32]> {
    let stem = name.strip_suffix(".vec")?;
    if stem.len() != 64 {
        return None;
    }
    let mut identity = [0_u8; 32];
    for (index, pair) in stem.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).ok()?;
        identity[index] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(identity)
}

fn parse_hexadecimal_32(name: &str) -> Option<[u8; 32]> {
    if name.len() != 64 {
        return None;
    }
    let mut value = [0_u8; 32];
    for (index, pair) in name.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).ok()?;
        value[index] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(value)
}

fn hexadecimal(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn is_unit_vector(values: &[f32]) -> bool {
    let norm_squared = values
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>();
    (norm_squared - 1.0).abs() <= 0.001
}

fn timestamp(time: std::time::SystemTime) -> u128 {
    time.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn write_hashed(file: &mut File, hasher: &mut blake3::Hasher, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    hasher.update(bytes);
    Ok(())
}

fn read_hashed(file: &mut File, hasher: &mut blake3::Hasher, bytes: &mut [u8]) -> io::Result<()> {
    file.read_exact(bytes)?;
    hasher.update(bytes);
    Ok(())
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "embedding cache path is not a directory",
            ));
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        Err(_) => {}
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

fn set_private_file_mode(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = options;
}

fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    fs::rename(source, destination)
}

fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

fn cache_size_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "embedding cache size overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Fixture {
        directory: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("fixture clock")
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "backend-embedding-cache-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir(&directory).expect("fixture directory");
            Self { directory }
        }

        fn cache(&self) -> EmbeddingCacheFile {
            EmbeddingCacheFile {
                directory: self.directory.clone(),
                recipe: [7; 32],
                dimensions: 2,
                bytes_used: 0,
                entries: BTreeMap::new(),
                recency: 0,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn exact_entries_survive_view_changes_and_cold_reopen() {
        let fixture = Fixture::new();
        let first_identity = [1; 32];
        let later_identity = [2; 32];
        let first_vector = [0.6, 0.8];
        let later_vector = [0.8, 0.6];
        let mut first_view = fixture.cache();
        first_view
            .store_batch(&[(first_identity, &first_vector)])
            .expect("store first view");

        // A second view adds a new input but leaves the absent old input
        // reusable; durable entries are not projection membership.
        first_view
            .store_batch(&[(later_identity, &later_vector)])
            .expect("store changed view");
        drop(first_view);

        let (bytes_used, entries, recency) = inventory(&fixture.directory).expect("inventory");
        let mut reopened = EmbeddingCacheFile {
            directory: fixture.directory.clone(),
            recipe: [7; 32],
            dimensions: 2,
            bytes_used,
            entries,
            recency,
        };
        assert_eq!(
            reopened.load(first_identity).as_deref(),
            Some(&first_vector[..])
        );
        assert_eq!(
            reopened.load(later_identity).as_deref(),
            Some(&later_vector[..])
        );
        assert!(reopened.load([3; 32]).is_none());
    }

    #[test]
    fn exact_identity_survives_recipe_switch_and_legacy_layout_migration() {
        let fixture = Fixture::new();
        let identity = [0x31; 32];
        let vector = [0.6, 0.8];
        let legacy_directory = fixture.directory.join(hexadecimal(&[7; 32]));
        fs::create_dir(&legacy_directory).expect("legacy recipe directory");
        let mut legacy = EmbeddingCacheFile {
            directory: legacy_directory,
            recipe: [7; 32],
            dimensions: 2,
            bytes_used: 0,
            entries: BTreeMap::new(),
            recency: 0,
        };
        legacy
            .store_batch(&[(identity, &vector)])
            .expect("store legacy recipe entry");
        drop(legacy);

        migrate_legacy_recipe_directories(&fixture.directory).expect("migrate cache layout");
        let (bytes_used, entries, recency) = inventory(&fixture.directory).expect("inventory");
        let mut reopened = EmbeddingCacheFile {
            directory: fixture.directory.clone(),
            recipe: [8; 32],
            dimensions: 2,
            bytes_used,
            entries,
            recency,
        };
        assert_eq!(
            reopened.load(identity).as_deref(),
            Some(&vector[..]),
            "input identity already binds the exact runtime recipe"
        );
    }

    #[test]
    fn global_cache_evicts_the_least_recently_used_exact_entry() {
        let fixture = Fixture::new();
        let first_identity = [0x41; 32];
        let second_identity = [0x42; 32];
        let third_identity = [0x43; 32];
        let mut cache = fixture.cache();
        cache
            .store_batch(&[(first_identity, &[0.6, 0.8])])
            .expect("store first");
        cache
            .store_batch(&[(second_identity, &[0.8, 0.6])])
            .expect("store second");
        cache
            .entries
            .get_mut(&first_identity)
            .expect("first entry")
            .last_used = 1;
        cache
            .entries
            .get_mut(&second_identity)
            .expect("second entry")
            .last_used = 2;
        // Force the byte cap to require one eviction without allocating a
        // half-gigabyte fixture on disk.
        cache.bytes_used = MAX_CACHE_BYTES;
        cache
            .store_batch(&[(third_identity, &[1.0, 0.0])])
            .expect("store third under cap");
        assert!(!cache.entry_path(&first_identity).exists());
        assert!(cache.entry_path(&second_identity).exists());
        assert!(cache.entry_path(&third_identity).exists());
        assert_eq!(cache.bytes_used, MAX_CACHE_BYTES);
    }

    #[test]
    fn corrupt_entries_are_withdrawn_as_cache_misses() {
        let fixture = Fixture::new();
        let identity = [9; 32];
        let mut cache = fixture.cache();
        cache
            .store_batch(&[(identity, &[1.0, 0.0])])
            .expect("store entry");
        let path = cache.entry_path(&identity);
        let mut bytes = fs::read(&path).expect("entry bytes");
        bytes[HEADER_BYTES] ^= 0x01;
        fs::write(&path, bytes).expect("corrupt entry");

        assert!(cache.load(identity).is_none());
        assert!(!path.exists());
    }

    #[test]
    fn only_finite_unit_vectors_are_persisted() {
        let fixture = Fixture::new();
        let mut cache = fixture.cache();
        assert!(cache.store_batch(&[([1; 32], &[f32::NAN, 0.0])]).is_err());
        assert!(cache.store_batch(&[([2; 32], &[1.0, 1.0])]).is_err());
        assert_eq!(
            fs::read_dir(&fixture.directory).expect("directory").count(),
            0
        );
    }
}

fn cache_data_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "embedding cache entry is invalid",
    )
}
