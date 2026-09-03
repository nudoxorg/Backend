use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use thiserror::Error;

pub const VERSION: &str = "1.17.0";

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
    #[error("gzip/tar archive rejected: {source}")]
    Archive {
        #[source]
        source: io::Error,
    },
    #[error("unsupported tar entry type {kind}")]
    Entry { kind: u8 },
    #[error("tar path rejected: {path}")]
    Path { path: String },
    #[error("required six.py was not found")]
    MissingSource,
    #[error("fixture filesystem operation failed: {source}")]
    Io {
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Purl {
    pub ecosystem: String,
    pub name: String,
    pub version: String,
}

impl Purl {
    pub fn parse(input: &str) -> Result<Self, Error> {
        let mut parts = input.split('@');
        let Some(left) = parts.next() else {
            return Err(Error::Purl {
                input: input.into(),
            });
        };
        let Some(version) = parts.next() else {
            return Err(Error::Purl {
                input: input.into(),
            });
        };
        if parts.next().is_some() || version != VERSION {
            return Err(Error::Purl {
                input: input.into(),
            });
        }
        let mut name = left.split(':');
        let Some(ecosystem) = name.next() else {
            return Err(Error::Purl {
                input: input.into(),
            });
        };
        let Some(name) = name.next() else {
            return Err(Error::Purl {
                input: input.into(),
            });
        };
        if ecosystem != "pypi" || name.is_empty() {
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

pub fn locate(purl: &Purl) -> Result<(String, String), Error> {
    let url = format!("https://pypi.org/pypi/{}/{}/json", purl.name, purl.version);
    let response = ureq::get(&url)
        .call()
        .map_err(|source| Error::Network { source })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Status { status });
    }
    let mut text = String::new();
    response
        .into_body()
        .into_reader()
        .read_to_string(&mut text)
        .map_err(|source| Error::Read {
            observed: text.len(),
            source,
        })?;
    let marker = format!("https://files.pythonhosted.org/packages/");
    let mut sdist = None;
    let mut wheel = None;
    for quoted in text.split('"') {
        if quoted.starts_with(&marker)
            && quoted.ends_with(".tar.gz")
            && quoted.contains("six-1.17.0")
        {
            sdist = Some(quoted.to_owned());
        }
        if quoted.ends_with(".whl") && quoted.contains("six-1.17.0") {
            wheel = Some(quoted.to_owned());
        }
    }
    match (sdist, wheel) {
        (Some(sdist), Some(wheel)) => Ok((sdist, wheel)),
        _ => Err(Error::MissingSource),
    }
}

pub fn download(url: &str, cap: usize, deadline: Instant) -> Result<Vec<u8>, Error> {
    let response = ureq::get(url)
        .call()
        .map_err(|source| Error::Network { source })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Status { status });
    }
    let mut reader = response
        .into_body()
        .into_reader()
        .take((cap.saturating_add(1)) as u64);
    let mut bytes = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Deadline {
                observed: bytes.len(),
            });
        }
        let mut chunk = [0_u8; 8192];
        let read = reader.read(&mut chunk).map_err(|source| Error::Read {
            observed: bytes.len(),
            source,
        })?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > cap {
            return Err(Error::Cap {
                cap,
                observed: bytes.len(),
            });
        }
    }
    Ok(bytes)
}

pub fn unpack(bytes: &[u8], root: &Path) -> Result<(), Error> {
    let mut decoder = GzDecoder::new(bytes);
    let mut tar = Vec::new();
    decoder
        .read_to_end(&mut tar)
        .map_err(|source| Error::Archive { source })?;
    let mut pending_override: Option<String> = None;
    let mut offset = 0;
    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let name_end = header[..100]
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(100);
        let name = std::str::from_utf8(&header[..name_end]).map_err(|_| Error::Path {
            path: "non-utf8".into(),
        })?;
        let size_text = std::str::from_utf8(&header[124..136])
            .map_err(|_| Error::Path { path: name.into() })?;
        let size = usize::from_str_radix(size_text.trim_matches('\0').trim(), 8)
            .map_err(|_| Error::Path { path: name.into() })?;
        let kind = header[156];
        let target = root.join(name);
        if !target.starts_with(root) {
            return Err(Error::Path { path: name.into() });
        }
        match kind {
            // POSIX pax extended/global extended headers: metadata records
            // for later entries. six's sdist carries them; the only record
            // this unpacker must honour is `path=` (renaming the next
            // entry), everything else is skipped with the entry itself.
            b'x' | b'g' => {
                let payload = tar
                    .get(offset + 512..offset + 512 + size)
                    .ok_or_else(|| Error::Path { path: name.into() })?;
                if kind == b'x'
                    && let Some(override_path) = pax_path_override(payload)?
                {
                    pending_override = Some(override_path);
                }
            }
            b'5' => std::fs::create_dir_all(&target).map_err(|source| Error::Io { source })?,
            0 | b'0' => {
                let destination = match pending_override.take() {
                    Some(override_path) => {
                        let destination = root.join(&override_path);
                        if !destination.starts_with(root) {
                            return Err(Error::Path {
                                path: override_path,
                            });
                        }
                        destination
                    }
                    None => target,
                };
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent).map_err(|source| Error::Io { source })?;
                }
                std::fs::write(
                    &destination,
                    tar.get(offset + 512..offset + 512 + size)
                        .ok_or_else(|| Error::Path { path: name.into() })?,
                )
                .map_err(|source| Error::Io { source })?;
            }
            other => return Err(Error::Entry { kind: other }),
        }
        pending_override = None;
        offset += 512 + size.div_ceil(512) * 512;
    }
    Ok(())
}

/// Reads the `path=` record of one pax extended-header payload, if any.
/// Records are `<len> <key>=<value>\n`; the length counts the whole record
/// including its digits and space.
fn pax_path_override(payload: &[u8]) -> Result<Option<String>, Error> {
    let mut cursor = 0_usize;
    while cursor < payload.len() {
        let digits_end = payload[cursor..]
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(|| Error::Path {
                path: "pax record".into(),
            })?
            + cursor;
        let length_text =
            std::str::from_utf8(&payload[cursor..digits_end]).map_err(|_| Error::Path {
                path: "pax length".into(),
            })?;
        let length: usize = length_text.parse().map_err(|_| Error::Path {
            path: length_text.into(),
        })?;
        if length == 0 || cursor + length > payload.len() {
            return Err(Error::Path {
                path: "pax length out of range".into(),
            });
        }
        let record = &payload[cursor..cursor + length];
        let remainder = &record[digits_end - cursor + 1..];
        if remainder.starts_with(b"path=") {
            let value =
                std::str::from_utf8(&remainder[b"path=".len()..]).map_err(|_| Error::Path {
                    path: "pax path".into(),
                })?;
            return Ok(Some(value.trim_end_matches('\n').to_owned()));
        }
        cursor += length;
    }
    Ok(None)
}

pub fn find_six(root: &Path) -> Result<PathBuf, Error> {
    fn walk(path: &Path) -> io::Result<Option<PathBuf>> {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()) == Some("six.py") {
                return Ok(Some(path));
            }
            if path.is_dir() {
                if let Some(found) = walk(&path)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }
    walk(root)
        .map_err(|source| Error::Io { source })?
        .ok_or(Error::MissingSource)
}

pub fn fresh_dir(label: &str) -> Result<PathBuf, Error> {
    let path = std::env::temp_dir().join(format!("nudox-python-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&path).map_err(|source| Error::Io { source })?;
    Ok(path)
}
