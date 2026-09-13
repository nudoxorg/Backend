//! Bounded executable identity cache and sampling.
use super::{ExecutableIdentity, FileStamp};
use crate::ProcessError;
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[cfg(unix)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExecutableCacheEntry {
    pub(super) path: PathBuf,
    pub(super) identity: ExecutableIdentity,
    pub(super) sample: Vec<u8>,
}

#[cfg(unix)]
const EXECUTABLE_CACHE_CAPACITY: usize = 64;
#[cfg(unix)]
const EXECUTABLE_SAMPLE_BYTES: usize = 256;
#[cfg(unix)]
static EXECUTABLE_CACHE: OnceLock<Mutex<VecDeque<ExecutableCacheEntry>>> = OnceLock::new();

#[cfg(unix)]
fn executable_cache() -> &'static Mutex<VecDeque<ExecutableCacheEntry>> {
    EXECUTABLE_CACHE.get_or_init(|| Mutex::new(VecDeque::new()))
}

#[cfg(unix)]
pub(super) fn executable_cache_lookup(
    path: &Path,
    stamp: FileStamp,
) -> Option<ExecutableCacheEntry> {
    let mut cache = match executable_cache().lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    let index = cache
        .iter()
        .position(|entry| entry.path == path && entry.identity.stamp == stamp)?;
    let entry = cache.remove(index)?;
    let cached = entry.clone();
    cache.push_front(entry);
    Some(cached)
}

#[cfg(unix)]
pub(super) fn executable_cache_insert(path: &Path, identity: ExecutableIdentity, sample: Vec<u8>) {
    let mut cache = match executable_cache().lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    cache.retain(|entry| entry.path != path || entry.identity.stamp != identity.stamp);
    cache.push_front(ExecutableCacheEntry {
        path: path.to_owned(),
        identity,
        sample,
    });
    cache.truncate(EXECUTABLE_CACHE_CAPACITY);
}

#[cfg(unix)]
pub(super) fn sample_file(file: &mut File, length: u64) -> Result<Vec<u8>, ProcessError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ProcessError::Io)?;
    let full_sample_limit = (EXECUTABLE_SAMPLE_BYTES as u64).saturating_mul(2);
    let head_length = if length <= full_sample_limit {
        length
    } else {
        EXECUTABLE_SAMPLE_BYTES as u64
    };
    let mut sample = Vec::with_capacity(
        usize::try_from(head_length).map_err(|_| ProcessError::Io)?
            + if length > full_sample_limit {
                EXECUTABLE_SAMPLE_BYTES
            } else {
                0
            },
    );
    file.take(head_length)
        .read_to_end(&mut sample)
        .map_err(|_| ProcessError::Io)?;
    if length > full_sample_limit {
        let tail_length = EXECUTABLE_SAMPLE_BYTES as u64;
        file.seek(SeekFrom::Start(length - tail_length))
            .map_err(|_| ProcessError::Io)?;
        let mut tail = Vec::with_capacity(EXECUTABLE_SAMPLE_BYTES);
        file.take(tail_length)
            .read_to_end(&mut tail)
            .map_err(|_| ProcessError::Io)?;
        sample.extend_from_slice(&tail);
    }
    Ok(sample)
}

#[cfg(unix)]
pub(super) fn sample_bytes(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() <= EXECUTABLE_SAMPLE_BYTES.saturating_mul(2) {
        return bytes.to_vec();
    }
    let mut sample = Vec::with_capacity(EXECUTABLE_SAMPLE_BYTES.saturating_mul(2));
    sample.extend_from_slice(&bytes[..EXECUTABLE_SAMPLE_BYTES]);
    sample.extend_from_slice(&bytes[bytes.len() - EXECUTABLE_SAMPLE_BYTES..]);
    sample
}
