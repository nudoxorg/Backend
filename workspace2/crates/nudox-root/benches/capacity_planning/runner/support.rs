//! Shared safe fixed-slice, filesystem accounting, and path resolution helpers.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::BenchmarkError;

pub(crate) fn directory_bytes(directory: &Path) -> Result<u64, BenchmarkError> {
    let mut total = 0_u64;
    for entry in fs::read_dir(directory).map_err(|source| BenchmarkError::ReadDirectory {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(BenchmarkError::ReadDirectoryEntry)?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| BenchmarkError::FileType {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            total = total
                .checked_add(directory_bytes(&path)?)
                .ok_or(BenchmarkError::ByteCountOverflow)?;
        } else if file_type.is_file() {
            let bytes = entry
                .metadata()
                .map_err(|source| BenchmarkError::Metadata {
                    path: path.clone(),
                    source,
                })?
                .len();
            total = total
                .checked_add(bytes)
                .ok_or(BenchmarkError::ByteCountOverflow)?;
        }
    }
    Ok(total)
}

pub(crate) fn fixed_prefix<T>(values: &[T], observed: usize) -> Result<&[T], BenchmarkError> {
    values
        .get(..observed)
        .ok_or(BenchmarkError::FixedSliceLength {
            observed,
            capacity: values.len(),
        })
}

pub(crate) fn find_rustc() -> Result<PathBuf, BenchmarkError> {
    let Some(path) = std::env::var_os("PATH") else {
        return Err(BenchmarkError::RustcNotFound);
    };
    for directory in std::env::split_paths(&path) {
        if !directory.is_absolute() {
            continue;
        }
        let candidate = directory.join("rustc");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(BenchmarkError::RustcNotFound)
}

pub(crate) fn absolute_path(path: PathBuf) -> Result<PathBuf, BenchmarkError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(BenchmarkError::RelativeRustc { path })
    }
}

pub(crate) fn absolute_result_path(path: &Path) -> Result<PathBuf, BenchmarkError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .map_err(BenchmarkError::CurrentDirectory)?
        .join(path))
}
