//! Crash-safe membership for the optional Qdrant projection.
//!
//! This state tracks row residences only. It is deliberately separate from
//! the exact-input embedding cache: old vectors can remain useful across view
//! changes while points omitted from a new complete view must be withdrawn.

use backend_extension_qdrant as qdrant;
use backend_version::WorkspaceRoot;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"BEMQP001";
const VERSION: u8 = 1;
const FIXED_BYTES: usize = 8 + 1 + 32 + 32 + 32 + 4;
const CHECKSUM_BYTES: usize = 32;
const MAX_ROWS: usize = 65_536;
const MAX_STAGED_ROWS: usize = MAX_ROWS * 2;
const MAX_ROW_KEY_BYTES: usize = 4096;
const MAX_STATE_BYTES: usize = 16 * 1024 * 1024;
const MAX_STATE_FILES: usize = 64;
const STATE_PARENT: &str = "qdrant-projection";
const STATE_DOMAIN: &str = "backend.local-service.qdrant-projection-state.v1";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Durable known membership for one exact workspace and Qdrant recipe.
pub(super) struct ProjectionState {
    path: PathBuf,
    workspace: WorkspaceRoot,
    recipe: qdrant::Recipe,
    provider_scope: [u8; 32],
    row_keys: Vec<String>,
}

impl ProjectionState {
    /// Loads the current workspace/recipe manifest, if durable storage exists.
    pub(super) fn open(
        workspace: WorkspaceRoot,
        recipe: qdrant::Recipe,
        provider_scope: [u8; 32],
    ) -> Option<Self> {
        let root = std::env::var_os(backend_runtime::DATA_ENV).map(PathBuf::from)?;
        if !root.is_absolute() {
            return None;
        }
        let directory = root.join("cache").join(STATE_PARENT);
        Self::open_in_directory(&directory, workspace, recipe, provider_scope)
    }

    pub(super) fn open_in_directory(
        directory: &Path,
        workspace: WorkspaceRoot,
        recipe: qdrant::Recipe,
        provider_scope: [u8; 32],
    ) -> Option<Self> {
        create_private_directory(directory).ok()?;
        let path = directory.join(format!(
            "{}-{}-{}.state",
            hexadecimal(workspace.as_bytes()),
            hexadecimal(recipe.as_bytes()),
            hexadecimal(&provider_scope)
        ));
        let row_keys = match read_state(&path, workspace, recipe, provider_scope) {
            Ok(rows) => rows,
            Err(()) => {
                let _ = fs::remove_file(&path);
                Vec::new()
            }
        };
        let state = Self {
            path,
            workspace,
            recipe,
            provider_scope,
            row_keys,
        };
        state.prune_old_manifests();
        Some(state)
    }

    /// Exact rows included by the last committed or staged mutation.
    pub(super) fn row_keys(&self) -> &[String] {
        &self.row_keys
    }

    /// Records old and target rows before Qdrant can receive a partial upsert.
    pub(super) fn stage(&mut self, target: &[String]) -> io::Result<()> {
        let target = validate_rows(target)?;
        let rows = self
            .row_keys
            .iter()
            .cloned()
            .chain(target)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let rows = validate_staged_rows(&rows)?;
        self.persist(&rows)?;
        self.row_keys = rows;
        Ok(())
    }

    /// Commits only the verified live projection after stale points are deleted.
    pub(super) fn commit(&mut self, target: &[String]) -> io::Result<()> {
        let rows = validate_rows(target)?;
        self.persist(&rows)?;
        self.row_keys = rows;
        Ok(())
    }

    /// Rebuilds this manifest's exact typed point residences.
    pub(super) fn residences(
        &self,
    ) -> Result<Vec<qdrant::PointResidence>, qdrant::HttpProviderError> {
        self.row_keys
            .iter()
            .map(|row| qdrant::PointResidence::for_row(self.workspace, self.recipe, row))
            .collect()
    }

    pub(super) const fn workspace(&self) -> WorkspaceRoot {
        self.workspace
    }

    pub(super) const fn recipe(&self) -> qdrant::Recipe {
        self.recipe
    }

    fn persist(&self, rows: &[String]) -> io::Result<()> {
        let encoded = encode_state(self.workspace, self.recipe, self.provider_scope, rows)?;
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = self.path.with_extension(format!(
            "{}-{}-{sequence}.tmp",
            std::process::id(),
            hexadecimal(self.workspace.as_bytes())
        ));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            set_private_file_mode(&mut options);
            let mut file = options.open(&temporary)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            replace_file(&temporary, &self.path)?;
            sync_parent(&self.path);
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn prune_old_manifests(&self) {
        let Some(directory) = self.path.parent() else {
            return;
        };
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        let mut manifests = Vec::new();
        for entry in entries.take(MAX_STATE_FILES.saturating_add(1)).flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "tmp")
                && entry.file_type().is_ok_and(|kind| kind.is_file())
            {
                let _ = fs::remove_file(path);
            } else if path != self.path
                && path
                    .extension()
                    .is_some_and(|extension| extension == "state")
                && entry.file_type().is_ok_and(|kind| kind.is_file())
            {
                let last_used = entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(timestamp)
                    .unwrap_or_default();
                manifests.push((path, last_used));
            }
        }
        if manifests.len() >= MAX_STATE_FILES {
            manifests.sort_by_key(|(_, last_used)| *last_used);
            let remove_count = manifests
                .len()
                .saturating_add(1)
                .saturating_sub(MAX_STATE_FILES);
            for (path, _) in manifests.into_iter().take(remove_count) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn validate_rows(rows: &[String]) -> io::Result<Vec<String>> {
    validate_rows_with_limit(rows, MAX_ROWS)
}

fn timestamp(time: std::time::SystemTime) -> u128 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn validate_staged_rows(rows: &[String]) -> io::Result<Vec<String>> {
    validate_rows_with_limit(rows, MAX_STAGED_ROWS)
}

fn validate_rows_with_limit(rows: &[String], row_limit: usize) -> io::Result<Vec<String>> {
    if rows.len() > row_limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "projection row limit",
        ));
    }
    let mut rows = rows.to_vec();
    rows.sort_unstable();
    rows.dedup();
    let encoded_size = rows
        .iter()
        .try_fold(FIXED_BYTES + CHECKSUM_BYTES, |size, row| {
            if !valid_row_key(row) {
                return None;
            }
            size.checked_add(4)?.checked_add(row.len())
        });
    if encoded_size.is_none_or(|size| size > MAX_STATE_BYTES) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "projection state limit",
        ));
    }
    Ok(rows)
}

fn valid_row_key(row: &str) -> bool {
    let Some((kind, digest)) = row.split_once(':') else {
        return false;
    };
    matches!(kind, "package" | "symbol" | "object")
        && digest.len() == 64
        && digest
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn encode_state(
    workspace: WorkspaceRoot,
    recipe: qdrant::Recipe,
    provider_scope: [u8; 32],
    rows: &[String],
) -> io::Result<Vec<u8>> {
    let rows = validate_staged_rows(rows)?;
    let capacity = validate_rows_size(&rows).ok_or_else(state_data_error)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| io::Error::other("projection state allocation failed"))?;
    output.extend_from_slice(MAGIC);
    output.push(VERSION);
    output.extend_from_slice(workspace.as_bytes());
    output.extend_from_slice(recipe.as_bytes());
    output.extend_from_slice(&provider_scope);
    output.extend_from_slice(
        &u32::try_from(rows.len())
            .map_err(|_| state_data_error())?
            .to_be_bytes(),
    );
    for row in rows {
        let bytes = row.as_bytes();
        output.extend_from_slice(
            &u32::try_from(bytes.len())
                .map_err(|_| state_data_error())?
                .to_be_bytes(),
        );
        output.extend_from_slice(bytes);
    }
    let mut hasher = blake3::Hasher::new_derive_key(STATE_DOMAIN);
    hasher.update(&output);
    output.extend_from_slice(hasher.finalize().as_bytes());
    Ok(output)
}

fn read_state(
    path: &Path,
    workspace: WorkspaceRoot,
    recipe: qdrant::Recipe,
    provider_scope: [u8; 32],
) -> Result<Vec<String>, ()> {
    let link_metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if !link_metadata.file_type().is_file()
        || link_metadata.len() > u64::try_from(MAX_STATE_BYTES).map_err(|_| ())?
    {
        return Err(());
    }
    let mut file = File::open(path).map_err(|_| ())?;
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(link_metadata.len()).map_err(|_| ())?)
        .map_err(|_| ())?;
    file.take(
        u64::try_from(MAX_STATE_BYTES)
            .map_err(|_| ())?
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .map_err(|_| ())?;
    if bytes.len() < FIXED_BYTES + CHECKSUM_BYTES
        || bytes.len() > MAX_STATE_BYTES
        || &bytes[..8] != MAGIC
        || bytes[8] != VERSION
        || bytes[9..41] != *workspace.as_bytes()
        || bytes[41..73] != *recipe.as_bytes()
        || bytes[73..105] != provider_scope
    {
        return Err(());
    }
    let checksum_at = bytes.len().checked_sub(CHECKSUM_BYTES).ok_or(())?;
    let mut hasher = blake3::Hasher::new_derive_key(STATE_DOMAIN);
    hasher.update(&bytes[..checksum_at]);
    if bytes[checksum_at..] != *hasher.finalize().as_bytes() {
        return Err(());
    }
    let count = usize::try_from(u32::from_be_bytes(
        bytes[105..109].try_into().map_err(|_| ())?,
    ))
    .map_err(|_| ())?;
    if count > MAX_STAGED_ROWS {
        return Err(());
    }
    let mut offset = FIXED_BYTES;
    let mut rows = Vec::new();
    rows.try_reserve_exact(count).map_err(|_| ())?;
    let mut prior: Option<String> = None;
    for _ in 0..count {
        let length_end = offset.checked_add(4).ok_or(())?;
        if length_end > checksum_at {
            return Err(());
        }
        let length = usize::try_from(u32::from_be_bytes(
            bytes[offset..length_end].try_into().map_err(|_| ())?,
        ))
        .map_err(|_| ())?;
        offset = length_end;
        if length == 0 || length > MAX_ROW_KEY_BYTES {
            return Err(());
        }
        let end = offset.checked_add(length).ok_or(())?;
        if end > checksum_at {
            return Err(());
        }
        let row = String::from_utf8(bytes[offset..end].to_vec()).map_err(|_| ())?;
        if !valid_row_key(&row) || prior.as_ref().is_some_and(|prior| prior >= &row) {
            return Err(());
        }
        prior = Some(row.clone());
        rows.push(row);
        offset = end;
    }
    if offset != checksum_at {
        return Err(());
    }
    Ok(rows)
}

fn validate_rows_size(rows: &[String]) -> Option<usize> {
    rows.iter()
        .try_fold(FIXED_BYTES + CHECKSUM_BYTES, |size, row| {
            size.checked_add(4)?.checked_add(row.len())
        })
        .filter(|size| *size <= MAX_STATE_BYTES)
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

fn create_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "state path is not a directory",
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

fn state_data_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "projection state is invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_membership_accepts_only_canonical_typed_row_keys() {
        for kind in ["package", "symbol", "object"] {
            let key = format!("{kind}:{}", "a".repeat(64));
            assert!(valid_row_key(&key));
        }
        assert!(!valid_row_key(&format!("symbol:{}", "A".repeat(64))));
        assert!(!valid_row_key(&format!("other:{}", "a".repeat(64))));
        assert!(!valid_row_key(&format!("object:{}", "a".repeat(63))));
    }
}
