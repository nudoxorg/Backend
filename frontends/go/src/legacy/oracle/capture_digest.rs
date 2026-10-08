//! Stable file digests shared only while constructing one authority witness.

use std::{
    collections::BTreeMap,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CapturedFile {
    File { bytes: u64, digest: [u8; 32] },
    Unavailable,
    Limit,
}

pub(super) struct CaptureLimits {
    pub(super) file_bytes: u64,
    pub(super) total_bytes: u64,
}

#[derive(Clone, PartialEq)]
struct FileStamp {
    identity: backend_platform::FileIdentity,
    bytes: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
}

struct CachedDigest {
    stamp: FileStamp,
    digest: [u8; 32],
}

/// Never stored on a witness or reused by a later recapture. Each consumer
/// independently admits its path and charges its own byte budget, even on a hit.
#[derive(Default)]
pub(super) struct CaptureScratch {
    files: BTreeMap<PathBuf, CachedDigest>,
    #[cfg(test)]
    pub(super) hash_bytes: u64,
    #[cfg(test)]
    pub(super) hash_files: usize,
    #[cfg(test)]
    pub(super) cancel_after_bytes: Option<(u64, std::sync::Arc<AtomicBool>)>,
    #[cfg(test)]
    after_chunk: Option<Box<dyn FnMut()>>,
}

pub(super) fn is_cancelled(cancelled: Option<&AtomicBool>) -> bool {
    cancelled.is_some_and(|token| token.load(Ordering::Acquire))
}

pub(super) fn clean_components(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| !matches!(part, Component::ParentDir | Component::CurDir))
}

pub(super) fn admitted_path(path: &Path, roots: &[PathBuf]) -> bool {
    if !path.is_absolute()
        || !clean_components(path)
        || !roots.iter().any(|root| path.starts_with(root))
    {
        return false;
    }
    path.canonicalize()
        .is_ok_and(|resolved| roots.iter().any(|root| resolved.starts_with(root)))
}

fn file_stamp(file: &std::fs::File) -> std::io::Result<FileStamp> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other(
            "authority input is not a regular file",
        ));
    }
    Ok(FileStamp {
        identity: backend_platform::FileIdentity::of_file(file)?,
        bytes: metadata.len(),
        modified: metadata.modified()?,
        #[cfg(unix)]
        changed: {
            use std::os::unix::fs::MetadataExt as _;
            (metadata.ctime(), metadata.ctime_nsec())
        },
    })
}

impl CaptureScratch {
    pub(super) fn capture_file(
        &mut self,
        path: &Path,
        roots: &[PathBuf],
        total: &mut u64,
        limits: CaptureLimits,
        cancelled: Option<&AtomicBool>,
    ) -> CapturedFile {
        if is_cancelled(cancelled) || !admitted_path(path, roots) {
            return CapturedFile::Unavailable;
        }
        let Ok(resolved) = path.canonicalize() else {
            return CapturedFile::Unavailable;
        };
        let Ok(mut file) = backend_platform::durability::open_regular_file_nofollow(&resolved)
        else {
            return CapturedFile::Unavailable;
        };
        let Ok(before) = file_stamp(&file) else {
            return CapturedFile::Unavailable;
        };
        if backend_platform::FileIdentity::of_path_nofollow(&resolved).ok() != Some(before.identity)
        {
            return CapturedFile::Unavailable;
        }
        let bytes = before.bytes;
        if bytes > limits.file_bytes || total.saturating_add(bytes) > limits.total_bytes {
            return CapturedFile::Limit;
        }

        // This stamp is only a same-construction guard. No stamp survives this
        // scratch's lifetime, so every later full capture hashes fresh contents.
        let content = if let Some(cached) = self
            .files
            .get(&resolved)
            .filter(|cached| cached.stamp == before)
        {
            cached.digest
        } else {
            let mut digest = Sha256::new();
            let mut read = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            #[cfg(test)]
            {
                self.hash_files += 1;
            }
            loop {
                if is_cancelled(cancelled) {
                    return CapturedFile::Unavailable;
                }
                let Ok(count) = file.read(&mut buffer) else {
                    return CapturedFile::Unavailable;
                };
                if count == 0 {
                    break;
                }
                read = read.saturating_add(count as u64);
                if read > bytes {
                    return CapturedFile::Unavailable;
                }
                digest.update(&buffer[..count]);
                #[cfg(test)]
                {
                    self.hash_bytes += count as u64;
                    if let Some((threshold, token)) = &self.cancel_after_bytes {
                        if self.hash_bytes >= *threshold {
                            token.store(true, Ordering::Release);
                        }
                    }
                    if let Some(after_chunk) = &mut self.after_chunk {
                        after_chunk();
                    }
                }
            }
            if read != bytes {
                return CapturedFile::Unavailable;
            }
            digest.finalize().into()
        };
        if is_cancelled(cancelled)
            || file_stamp(&file).ok().as_ref() != Some(&before)
            || path.canonicalize().ok().as_ref() != Some(&resolved)
            || !admitted_path(path, roots)
            || backend_platform::FileIdentity::of_path_nofollow(&resolved).ok()
                != Some(before.identity)
        {
            return CapturedFile::Unavailable;
        }
        self.files.insert(
            resolved,
            CachedDigest {
                stamp: before,
                digest: content,
            },
        );
        *total = total.saturating_add(bytes);
        CapturedFile::File {
            bytes,
            digest: content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(
        scratch: &mut CaptureScratch,
        path: &Path,
        root: &Path,
        total: &mut u64,
        cancelled: Option<&AtomicBool>,
    ) -> CapturedFile {
        scratch.capture_file(
            path,
            &[root.to_path_buf()],
            total,
            CaptureLimits {
                file_bytes: 1024 * 1024,
                total_bytes: 2 * 1024 * 1024,
            },
            cancelled,
        )
    }

    #[test]
    fn same_capture_hashes_content_once_but_charges_each_consumer() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("selected.go");
        let bytes = vec![b'a'; 128 * 1024 + 17];
        std::fs::write(&path, &bytes).unwrap();
        let mut scratch = CaptureScratch::default();
        let mut local_total = 0;
        let local = capture(&mut scratch, &path, &root, &mut local_total, None);
        let mut selected_total = 0;
        let selected = capture(&mut scratch, &path, &root, &mut selected_total, None);
        assert!(
            matches!(local, CapturedFile::File { digest, .. } if digest == <[u8;32]>::from(Sha256::digest(&bytes)))
        );
        assert_eq!(local, selected);
        assert_eq!(scratch.hash_files, 1);
        assert_eq!(scratch.hash_bytes, bytes.len() as u64);
        assert_eq!(local_total, bytes.len() as u64);
        assert_eq!(selected_total, bytes.len() as u64);
    }

    #[test]
    fn every_new_capture_hashes_same_length_drift_even_with_restored_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("selected.go");
        std::fs::write(&path, b"package dep\nconst Value = 1\n").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut first = CaptureScratch::default();
        let before = capture(&mut first, &path, &root, &mut 0, None);
        std::fs::write(&path, b"package dep\nconst Value = 2\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let mut second = CaptureScratch::default();
        let after = capture(&mut second, &path, &root, &mut 0, None);
        assert_ne!(before, after);
        assert_eq!(first.hash_files, 1);
        assert_eq!(second.hash_files, 1);
        assert_eq!(first.hash_bytes, 28);
        assert_eq!(second.hash_bytes, 28);
    }

    #[test]
    fn a_cache_hit_does_not_bypass_an_independent_byte_limit_or_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("selected.go");
        std::fs::write(&path, b"1234").unwrap();
        let mut scratch = CaptureScratch::default();
        assert!(matches!(
            capture(&mut scratch, &path, &root, &mut 0, None),
            CapturedFile::File { .. }
        ));
        let mut total = 2 * 1024 * 1024 - 3;
        assert_eq!(
            capture(&mut scratch, &path, &root, &mut total, None),
            CapturedFile::Limit
        );
        assert_eq!(total, 2 * 1024 * 1024 - 3);
        assert_eq!(
            scratch.capture_file(
                &path,
                &[root.clone()],
                &mut 0,
                CaptureLimits {
                    file_bytes: 3,
                    total_bytes: 100
                },
                None
            ),
            CapturedFile::Limit
        );
        assert_eq!(
            capture(
                &mut scratch,
                &path,
                &root,
                &mut 0,
                Some(&AtomicBool::new(true))
            ),
            CapturedFile::Unavailable
        );
        assert_eq!(scratch.hash_bytes, 4);
        assert_eq!(scratch.hash_files, 1);
    }

    #[test]
    fn cancellation_after_a_chunk_never_inserts_a_partial_digest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("selected.go");
        std::fs::write(&path, vec![b'x'; 128 * 1024]).unwrap();
        let token = std::sync::Arc::new(AtomicBool::new(false));
        let mut scratch = CaptureScratch {
            cancel_after_bytes: Some((64 * 1024, token.clone())),
            ..Default::default()
        };
        let mut total = 0;
        assert_eq!(
            capture(&mut scratch, &path, &root, &mut total, Some(&token)),
            CapturedFile::Unavailable
        );
        assert_eq!(scratch.hash_bytes, 64 * 1024);
        assert!(scratch.files.is_empty());
        assert_eq!(total, 0);
        token.store(false, Ordering::Release);
        scratch.cancel_after_bytes = None;
        assert!(matches!(
            capture(&mut scratch, &path, &root, &mut total, Some(&token)),
            CapturedFile::File { bytes: 131072, .. }
        ));
        assert_eq!(scratch.hash_files, 2);
        assert_eq!(scratch.hash_bytes, 192 * 1024);
    }

    #[test]
    fn same_inode_same_length_mutation_during_stream_is_not_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("selected.go");
        std::fs::write(&path, vec![b'a'; 128 * 1024]).unwrap();
        let identity = backend_platform::FileIdentity::of_path_nofollow(&path).unwrap();
        let mut mutate = Some(path.clone());
        let mut scratch = CaptureScratch {
            after_chunk: Some(Box::new(move || {
                if let Some(path) = mutate.take() {
                    std::fs::write(path, vec![b'b'; 128 * 1024]).unwrap();
                }
            })),
            ..Default::default()
        };
        let mut total = 0;
        assert_eq!(
            capture(&mut scratch, &path, &root, &mut total, None),
            CapturedFile::Unavailable
        );
        assert_eq!(
            identity,
            backend_platform::FileIdentity::of_path_nofollow(&path).unwrap()
        );
        assert_eq!(total, 0);
        assert!(scratch.files.is_empty());
        scratch.after_chunk = None;
        assert!(
            matches!(capture(&mut scratch, &path, &root, &mut total, None), CapturedFile::File { digest, .. }
            if digest == <[u8; 32]>::from(Sha256::digest(vec![b'b'; 128 * 1024])))
        );
        assert_eq!(scratch.hash_files, 2);
        assert_eq!(scratch.hash_bytes, 256 * 1024);
    }

    #[cfg(unix)]
    #[test]
    fn independent_root_admission_and_retargeted_symlinks_cannot_hit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let other = tempfile::tempdir().unwrap();
        let outside = other.path().canonicalize().unwrap();
        let first = root.join("first.go");
        let second = root.join("second.go");
        std::fs::write(&first, b"1111").unwrap();
        std::fs::write(&second, b"2222").unwrap();
        std::fs::write(outside.join("outside.go"), b"3333").unwrap();
        let link = root.join("selected.go");
        std::os::unix::fs::symlink(&first, &link).unwrap();
        let mut scratch = CaptureScratch::default();
        let before = capture(&mut scratch, &link, &root, &mut 0, None);
        assert_eq!(
            capture(&mut scratch, &first, &outside, &mut 0, None),
            CapturedFile::Unavailable
        );
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&second, &link).unwrap();
        let after = capture(&mut scratch, &link, &root, &mut 0, None);
        assert_ne!(before, after);
        assert_eq!(scratch.hash_files, 2);
        assert_eq!(scratch.hash_bytes, 8);
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(outside.join("outside.go"), &link).unwrap();
        assert_eq!(
            capture(&mut scratch, &link, &root, &mut 0, None),
            CapturedFile::Unavailable
        );
        assert_eq!(scratch.hash_bytes, 8);
    }
}
