use flate2::read::DeflateDecoder;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("malformed PURL: {input}")]
    Purl { input: String },
    #[error("network request failed: {source}")]
    Network {
        #[source]
        source: ureq::Error,
    },
    #[error("HTTP status {status}")]
    Status { status: u16 },
    #[error("response exceeded {cap} bytes after observing {observed}")]
    Cap { cap: usize, observed: usize },
    #[error("download deadline expired after observing {observed} bytes")]
    Deadline { observed: usize },
    #[error("response read failed after observing {observed} bytes: {source}")]
    Read {
        observed: usize,
        #[source]
        source: io::Error,
    },
    #[error("invalid proxy metadata: {field}")]
    Metadata { field: &'static str },
    #[error("zip archive rejected: {source}")]
    Archive {
        #[source]
        source: io::Error,
    },
    #[error("zip path rejected: {path}")]
    Path { path: String },
    #[error("unsupported zip compression method {method}")]
    Compression { method: u16 },
    #[error("required Go source was not found")]
    MissingSource,
    #[error("fixture filesystem operation failed: {source}")]
    Io {
        #[source]
        source: io::Error,
    },
    #[error("Go module dependency preparation failed with status {status}: {stderr}")]
    GoModule { status: String, stderr: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Purl {
    pub ecosystem: String,
    pub name: String,
    pub version: String,
}

impl Purl {
    pub fn parse(input: &str) -> Result<Self, Error> {
        let (left, version) = input.split_once('@').ok_or_else(|| Error::Purl {
            input: input.into(),
        })?;
        if version.is_empty() || version.contains('@') {
            return Err(Error::Purl {
                input: input.into(),
            });
        }
        let (ecosystem, name) = left.split_once(':').ok_or_else(|| Error::Purl {
            input: input.into(),
        })?;
        if ecosystem != "golang" || name.is_empty() || name.contains(':') {
            return Err(Error::Purl {
                input: input.into(),
            });
        }
        Ok(Self {
            ecosystem: ecosystem.into(),
            name: name.into(),
            version: version.into(),
        })
    }
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(text: &str) -> Result<[u8; 32], Error> {
    let bytes = text.trim().as_bytes();
    if bytes.len() != 64 {
        return Err(Error::Metadata { field: "ziphash" });
    }
    let mut out = [0; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(Error::Metadata { field: "ziphash" }),
        };
        out[i] = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Ok(out)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent()
}

pub fn locate(purl: &Purl) -> Result<(String, Option<[u8; 32]>), Error> {
    let base = format!(
        "https://proxy.golang.org/{}/@v/{}",
        proxy_path(&purl.name),
        purl.version
    );
    let info = agent()
        .get(&format!("{base}.info"))
        .call()
        .map_err(|source| Error::Network { source })?;
    let status = info.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Status { status });
    }
    let mut text = String::new();
    info.into_body()
        .into_reader()
        .take(64 * 1024)
        .read_to_string(&mut text)
        .map_err(|source| Error::Read {
            observed: text.len(),
            source,
        })?;
    if !text.contains(&format!("\"Version\":\"{}\"", purl.version)) {
        return Err(Error::Metadata { field: "Version" });
    }
    let hash = match agent().get(&format!("{base}.ziphash")).call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(404)) => return Ok((format!("{base}.zip"), None)),
        Err(source) => return Err(Error::Network { source }),
    };
    let status = hash.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Status { status });
    }
    let mut digest = String::new();
    hash.into_body()
        .into_reader()
        .take(128)
        .read_to_string(&mut digest)
        .map_err(|source| Error::Read {
            observed: digest.len(),
            source,
        })?;
    Ok((format!("{base}.zip"), Some(hex(&digest)?)))
}

/// Applies the Go module proxy's case-escaping rule.  Uppercase path bytes
/// are represented by `!` followed by their lowercase ASCII byte.
pub fn proxy_path(path: &str) -> String {
    path.chars()
        .flat_map(|character| {
            character
                .is_ascii_uppercase()
                .then_some('!')
                .into_iter()
                .chain(std::iter::once(character.to_ascii_lowercase()))
        })
        .collect()
}

pub fn download(url: &str, cap: usize, deadline: Instant) -> Result<Vec<u8>, Error> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|source| Error::Network { source })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Status { status });
    }
    let mut reader = response
        .into_body()
        .into_reader()
        .take(cap.saturating_add(1) as u64);
    let mut bytes = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Deadline {
                observed: bytes.len(),
            });
        }
        let mut chunk = [0; 8192];
        let read = reader.read(&mut chunk).map_err(|source| Error::Read {
            observed: bytes.len(),
            source,
        })?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > cap {
            return Err(Error::Cap {
                cap,
                observed: bytes.len(),
            });
        }
    }
}

fn escaped(path: &str) -> Result<(), Error> {
    if path.starts_with('/') || path.split(['/', '\\']).any(|part| part == "..") {
        return Err(Error::Path { path: path.into() });
    }
    Ok(())
}
fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Error> {
    bytes
        .get(at..at + 2)
        .and_then(|x| x.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| Error::Path {
            path: "truncated zip".into(),
        })
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Error> {
    bytes
        .get(at..at + 4)
        .and_then(|x| x.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| Error::Path {
            path: "truncated zip".into(),
        })
}

pub fn unpack(bytes: &[u8], root: &Path) -> Result<(), Error> {
    let mut eocd = None;
    for at in (0..bytes.len().saturating_sub(21)).rev() {
        if bytes[at..].starts_with(b"PK\x05\x06") {
            eocd = Some(at);
            break;
        }
    }
    let at = eocd.ok_or_else(|| Error::Path {
        path: "zip end record".into(),
    })?;
    let count = u16_at(bytes, at + 10)? as usize;
    let directory = u32_at(bytes, at + 16)? as usize;
    let mut pos = directory;
    for _ in 0..count {
        if !bytes
            .get(pos..)
            .is_some_and(|x| x.starts_with(b"PK\x01\x02"))
        {
            return Err(Error::Path {
                path: format!("central directory at {pos}, offset {directory}, entries {count}"),
            });
        }
        let method = u16_at(bytes, pos + 10)?;
        let compressed = u32_at(bytes, pos + 20)? as usize;
        let size = u32_at(bytes, pos + 24)? as usize;
        let name_len = u16_at(bytes, pos + 28)? as usize;
        let extra_len = u16_at(bytes, pos + 30)? as usize;
        let comment_len = u16_at(bytes, pos + 32)? as usize;
        let local = u32_at(bytes, pos + 42)? as usize;
        let name_end = pos + 46 + name_len;
        let name =
            std::str::from_utf8(bytes.get(pos + 46..name_end).ok_or_else(|| Error::Path {
                path: "zip name".into(),
            })?)
            .map_err(|_| Error::Path {
                path: "non-utf8".into(),
            })?;
        escaped(name)?;
        let local_name = u16_at(bytes, local + 26)? as usize;
        let local_extra = u16_at(bytes, local + 28)? as usize;
        let start = local + 30 + local_name + local_extra;
        let payload = bytes
            .get(start..start + compressed)
            .ok_or_else(|| Error::Path { path: name.into() })?;
        let mut data = Vec::with_capacity(size);
        match method {
            0 => data.extend_from_slice(payload),
            8 => {
                DeflateDecoder::new(payload)
                    .read_to_end(&mut data)
                    .map_err(|source| Error::Archive { source })?;
            }
            other => return Err(Error::Compression { method: other }),
        }
        if data.len() != size {
            return Err(Error::Path { path: name.into() });
        }
        if name.ends_with('/') {
            std::fs::create_dir_all(root.join(name)).map_err(|source| Error::Io { source })?;
        } else {
            let destination = root.join(name);
            if !destination.starts_with(root) {
                return Err(Error::Path { path: name.into() });
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|source| Error::Io { source })?;
            }
            std::fs::write(destination, data).map_err(|source| Error::Io { source })?;
        }
        pos = name_end + extra_len + comment_len;
    }
    Ok(())
}

pub fn find_primary(root: &Path, module: &str, relative: &str) -> Result<PathBuf, Error> {
    let path = root.join(module).join(relative);
    if path.is_file() {
        Ok(path)
    } else {
        Err(Error::MissingSource)
    }
}
pub fn fresh_dir(label: &str) -> Result<PathBuf, Error> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "nudox-go-{label}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).map_err(|source| Error::Io { source })?;
    Ok(path)
}

/// Assembles a proxy workspace for pre-module (+incompatible) releases.
/// Modern module archives already carry their own manifest and are untouched.
pub fn ensure_module(root: &Path, module: &str) -> Result<bool, Error> {
    let module_root = root.join(module);
    let manifest = module_root.join("go.mod");
    if manifest.is_file() {
        return Ok(false);
    }
    let module_path = module.split_once('@').map_or(module, |(path, _)| path);
    std::fs::write(&manifest, format!("module {module_path}\ngo 1.12\n"))
        .map_err(|source| Error::Io { source })?;
    Ok(true)
}

/// Resolves sums required by a downloaded module without changing the
/// pre-module synthesis behavior of [`ensure_module`].
pub fn prepare_module(root: &Path, module: &str) -> Result<(), Error> {
    let module_root = root.join(module);
    let compiler = std::env::var_os("COMPILER_GO_COMPILER").unwrap_or_else(|| "go".into());
    let output = std::process::Command::new(compiler)
        .args(["mod", "download"])
        .current_dir(module_root)
        .output()
        .map_err(|source| Error::Io { source })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::GoModule {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::proxy_path;

    #[test]
    fn proxy_escapes_uppercase_only() {
        assert_eq!(
            proxy_path("github.com/BurntSushi/toml"),
            "github.com/!burnt!sushi/toml"
        );
        assert_eq!(proxy_path("github.com/example"), "github.com/example");
        assert_eq!(proxy_path("x/v2.0"), "x/v2.0");
    }
}
