//! Shared test-only Roslyn oracle factory.
//!
//! Set both `NUDOX_CSHARP_ORACLE_DLL` and `NUDOX_CSHARP_ORACLE_SHA256` to use
//! a prebuilt helper. Partial or invalid explicit settings fail closed. With
//! neither setting present, the legacy restore/publish lane remains enabled;
//! it can use ambient package sources and is not hermetic.
//! Root sets the DLL from its validated realization output and validates the
//! receipt, source pins, and complete side-by-side inventory; this module
//! attests only the DLL.
//! The prebuilt path must already be canonical (on Windows, including any
//! `\\?\` prefix returned by `canonicalize`).

use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

const DLL_ENV: &str = "NUDOX_CSHARP_ORACLE_DLL";
const SHA256_ENV: &str = "NUDOX_CSHARP_ORACLE_SHA256";
const MAX_DLL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub(crate) enum OracleError {
    #[error("prebuilt mode requires both {DLL_ENV} and {SHA256_ENV}")]
    IncompleteConfig,
    #[error("prebuilt oracle setting is empty or invalid")]
    InvalidConfig,
    #[error("prebuilt oracle refused ({reason}): {path}")]
    Artifact { path: PathBuf, reason: &'static str },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("dotnet {operation} failed: {status}")]
    Dotnet {
        operation: &'static str,
        status: String,
    },
}

fn artifact_error(path: &Path, reason: &'static str) -> OracleError {
    OracleError::Artifact {
        path: path.to_path_buf(),
        reason,
    }
}

/// The prebuilt route keeps the original DLL path; the legacy route owns the
/// temporary directory containing its entire published runtime set.
pub(crate) struct OracleDll {
    path: PathBuf,
    digest: [u8; 32],
    _storage: OracleStorage,
}

enum OracleStorage {
    Prebuilt,
    Legacy { _directory: PrivateDirectory },
}

impl OracleDll {
    pub(crate) fn path_for_use(&self) -> Result<&Path, OracleError> {
        verify_digest(
            &self.path,
            self.digest,
            matches!(&self._storage, OracleStorage::Prebuilt),
        )?;
        Ok(&self.path)
    }
}

fn prebuilt_oracle(
    path: Option<OsString>,
    digest: Option<OsString>,
) -> Result<Option<OracleDll>, OracleError> {
    let (path, digest) = match (path, digest) {
        (None, None) => return Ok(None),
        (Some(path), Some(digest)) if !path.is_empty() && !digest.is_empty() => (path, digest),
        (Some(_), Some(_)) => return Err(OracleError::InvalidConfig),
        _ => return Err(OracleError::IncompleteConfig),
    };
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(artifact_error(&path, "path must be absolute"));
    }
    let digest = digest.to_str().ok_or(OracleError::InvalidConfig)?;
    if digest.len() != 64 || !digest.is_ascii() {
        return Err(OracleError::InvalidConfig);
    }
    let mut parsed = [0; 32];
    for (index, pair) in digest.as_bytes().chunks_exact(2).enumerate() {
        parsed[index] = u8::from_str_radix(
            std::str::from_utf8(pair).map_err(|_| OracleError::InvalidConfig)?,
            16,
        )
        .map_err(|_| OracleError::InvalidConfig)?;
    }
    verify_digest(&path, parsed, true)?;
    Ok(Some(OracleDll {
        path,
        digest: parsed,
        _storage: OracleStorage::Prebuilt,
    }))
}

fn select_or_publish(
    path: Option<OsString>,
    digest: Option<OsString>,
    publish: impl FnOnce() -> Result<OracleDll, OracleError>,
) -> Result<OracleDll, OracleError> {
    match prebuilt_oracle(path, digest)? {
        Some(oracle) => Ok(oracle),
        None => publish(),
    }
}

pub(crate) fn for_test(dotnet: &Path, test_name: &str) -> Result<OracleDll, OracleError> {
    select_or_publish(env::var_os(DLL_ENV), env::var_os(SHA256_ENV), || {
        publish_legacy(dotnet, test_name)
    })
}

fn regular_file(path: &Path, require_canonical: bool) -> Result<fs::Metadata, OracleError> {
    if !path.is_absolute() {
        return Err(artifact_error(path, "path must be absolute"));
    }
    if path
        .components()
        .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(artifact_error(path, "path has a dot component"));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(artifact_error(path, "path is a symlink"));
    }
    if !metadata.file_type().is_file() {
        return Err(artifact_error(path, "path is not a regular file"));
    }
    if require_canonical && path != fs::canonicalize(path)? {
        #[cfg(windows)]
        return Err(artifact_error(
            path,
            "path is not canonical; pass the Windows canonical path, including any \\\\?\\ prefix",
        ));
        #[cfg(not(windows))]
        return Err(artifact_error(
            path,
            "path is not canonical or traverses a symlink",
        ));
    }
    Ok(metadata)
}

fn sha256_file(path: &Path, require_canonical: bool) -> Result<[u8; 32], OracleError> {
    let before = regular_file(path, require_canonical)?;
    if before.len() == 0 {
        return Err(artifact_error(path, "file is empty"));
    }
    if before.len() > MAX_DLL_BYTES {
        return Err(artifact_error(path, "file exceeds 64 MiB"));
    }
    let file = File::open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.len() != before.len() {
        return Err(artifact_error(path, "file changed during admission"));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.take(MAX_DLL_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DLL_BYTES {
        return Err(artifact_error(path, "file exceeds 64 MiB"));
    }
    if bytes.len() as u64 != before.len()
        || regular_file(path, require_canonical)?.len() != before.len()
    {
        return Err(artifact_error(path, "file changed during admission"));
    }
    Ok(Sha256::digest(bytes).into())
}

fn verify_digest(
    path: &Path,
    expected: [u8; 32],
    require_canonical: bool,
) -> Result<(), OracleError> {
    if sha256_file(path, require_canonical)? != expected {
        return Err(artifact_error(path, "SHA-256 mismatch"));
    }
    Ok(())
}

struct PrivateDirectory(PathBuf);

impl PrivateDirectory {
    fn create(prefix: &str) -> Result<Self, OracleError> {
        let base = env::temp_dir();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = base.join(format!("{prefix}-{}-{now}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn publish_legacy(dotnet: &Path, test_name: &str) -> Result<OracleDll, OracleError> {
    let helper_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/legacy/helper");
    let directory = PrivateDirectory::create(&format!("nudox-csharp-{test_name}-publish"))?;
    let intermediate = directory.path().join("obj");
    let mut intermediate_arg = OsString::from("-p:BaseIntermediateOutputPath=");
    intermediate_arg.push(&intermediate);
    intermediate_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let output_base = directory.path().join("bin");
    let mut output_base_arg = OsString::from("-p:BaseOutputPath=");
    output_base_arg.push(&output_base);
    output_base_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let restored = Command::new(dotnet)
        .args(["restore", "oracle.csproj", "--locked-mode", "--nologo"])
        .arg(&intermediate_arg)
        .current_dir(&helper_dir)
        .status()?;
    if !restored.success() {
        return Err(OracleError::Dotnet {
            operation: "restore",
            status: restored.to_string(),
        });
    }
    let published = Command::new(dotnet)
        .args([
            "publish",
            "oracle.csproj",
            "-c",
            "Release",
            "--nologo",
            "--no-restore",
            "-p:UseSharedCompilation=false",
        ])
        .arg(&intermediate_arg)
        .arg(&output_base_arg)
        .arg("-o")
        .arg(directory.path())
        .current_dir(&helper_dir)
        .status()?;
    if !published.success() {
        return Err(OracleError::Dotnet {
            operation: "publish",
            status: published.to_string(),
        });
    }
    let path = directory.path().join("oracle.dll");
    let digest = sha256_file(&path, false)?;
    Ok(OracleDll {
        path,
        digest,
        _storage: OracleStorage::Legacy {
            _directory: directory,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{OracleError, PrivateDirectory, select_or_publish};
    use sha2::{Digest, Sha256};
    use std::ffi::OsString;
    use std::fs;

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn explicit_prebuilt_admission_fails_closed_and_rechecks_tampering()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = PrivateDirectory::create("nudox-csharp-admission-test")?;
        let source = directory.path().join("oracle.dll");
        let bytes = b"approved test bytes";
        fs::write(&source, bytes)?;
        let source = fs::canonicalize(source)?;

        let mut published = false;
        let result = select_or_publish(Some(source.as_os_str().to_owned()), None, || {
            published = true;
            Err(OracleError::InvalidConfig)
        });
        assert!(matches!(result, Err(OracleError::IncompleteConfig)));
        assert!(!published, "partial explicit config must not build");

        for invalid_hash in [
            OsString::from("not-a-sha256"),
            OsString::from("00".repeat(32)),
        ] {
            published = false;
            let result = select_or_publish(
                Some(source.as_os_str().to_owned()),
                Some(invalid_hash),
                || {
                    published = true;
                    Err(OracleError::InvalidConfig)
                },
            );
            assert!(matches!(
                result,
                Err(OracleError::InvalidConfig | OracleError::Artifact { .. })
            ));
            assert!(!published, "invalid explicit config must not build");
        }

        published = false;
        let result = select_or_publish(
            Some(OsString::from("relative/oracle.dll")),
            Some(OsString::from(sha256_hex(bytes))),
            || {
                published = true;
                Err(OracleError::InvalidConfig)
            },
        );
        assert!(matches!(result, Err(OracleError::Artifact { .. })));
        assert!(!published, "invalid path must not build");

        published = false;
        let oracle = select_or_publish(
            Some(source.as_os_str().to_owned()),
            Some(OsString::from(sha256_hex(bytes))),
            || {
                published = true;
                Err(OracleError::InvalidConfig)
            },
        )?;
        assert!(!published, "valid explicit config must not build");
        assert_eq!(oracle.path_for_use()?, source.as_path());

        fs::write(&source, b"tampered test bytes")?;
        assert!(matches!(
            oracle.path_for_use(),
            Err(OracleError::Artifact {
                reason: "SHA-256 mismatch",
                ..
            })
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn explicit_prebuilt_admission_rejects_symlinks_without_build()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;
        let directory = PrivateDirectory::create("nudox-csharp-symlink-test")?;
        let source = directory.path().join("real.dll");
        let link = directory.path().join("linked.dll");
        fs::write(&source, b"approved test bytes")?;
        symlink(&source, &link)?;
        let mut published = false;
        let result = select_or_publish(
            Some(link.as_os_str().to_owned()),
            Some(OsString::from(sha256_hex(b"approved test bytes"))),
            || {
                published = true;
                Err(OracleError::InvalidConfig)
            },
        );
        assert!(matches!(result, Err(OracleError::Artifact { .. })));
        assert!(!published, "symlink explicit config must not build");
        Ok(())
    }
}
