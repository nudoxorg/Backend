use flate2::read::GzDecoder;
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
    #[error("gzip/tar archive rejected: {source}")]
    Archive {
        #[source]
        source: io::Error,
    },
    #[error("unsupported tar entry type {kind}")]
    Entry { kind: u8 },
    #[error("tar path rejected: {path}")]
    Path { path: String },
    #[error("required primary module was not found")]
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
        if parts.next().is_some() || version.is_empty() {
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

/// One shared transport with a hard 30-second global timeout: ureq 3.x
/// carries timeouts on the agent config, not on individual requests.
fn transport() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent()
}

pub fn locate(purl: &Purl) -> Result<(String, [u8; 32], String), Error> {
    let url = format!("https://pypi.org/pypi/{}/{}/json", purl.name, purl.version);
    let response = transport()
        .get(&url)
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
        .take(4 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|source| Error::Read {
            observed: text.len(),
            source,
        })?;
    let marker = format!("https://files.pythonhosted.org/packages/");
    let needle = format!("{}-{}", purl.name, purl.version).to_ascii_lowercase();
    let mut sdist = None;
    let mut sdist_digest = None;
    let mut wheel = None;
    for quoted in text.split('"') {
        let lowered = quoted.to_ascii_lowercase();
        if quoted.starts_with(&marker) && quoted.ends_with(".tar.gz") && lowered.contains(&needle) {
            sdist = Some(quoted.to_owned());
        }
        if quoted.ends_with(".whl") && lowered.contains(&needle) {
            wheel = Some(quoted.to_owned());
        }
    }
    // The pinned digest is located straight from the JSON's own sdist
    // record, so the later download can be verified against PyPI's
    // declared bytes rather than only the transport's word. Each entry
    // lists `digests` before its terminal `url`, so the LAST `digests`
    // before the sdist URL — bounded to one entry — is the sdist's own.
    if let Some(sdist_url) = &sdist {
        let record_start = text.find(sdist_url.as_str());
        if let Some(record_start) = record_start {
            let window = &text[..record_start];
            if let Some(digest_marker) = window.rfind("\"digests\"") {
                if record_start - digest_marker <= 2048 {
                    let digests = &text[digest_marker..record_start];
                    if let Some(sha_marker) = digests.find("\"sha256\"") {
                        let after = &digests[sha_marker + "\"sha256\"".len()..];
                        let hex_start = after.find('"').map_or(0, |offset| offset + 1);
                        let hex = &after[hex_start..];
                        if let Some(hex_end) = hex.find('"') {
                            if hex_end == 64 {
                                sdist_digest = Some(decode_hex64(&hex[..64])?);
                            }
                        }
                    }
                }
            }
        }
    }
    match (sdist, sdist_digest, wheel) {
        (Some(sdist), Some(sdist_digest), Some(wheel)) => Ok((sdist, sdist_digest, wheel)),
        _ => Err(Error::MissingSource),
    }
}

/// Decodes 64 hexadecimal characters into the 32 digest bytes.
fn decode_hex64(text: &str) -> Result<[u8; 32], Error> {
    fn hex_digit(byte: u8) -> Result<u8, Error> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(Error::Path {
                path: "sha256 hex".into(),
            }),
        }
    }
    let bytes = text.as_bytes();
    let mut out = [0_u8; 32];
    for (index, pair) in bytes.chunks(2).enumerate() {
        out[index] = hex_digit(pair[0])? << 4 | hex_digit(pair[1])?;
    }
    Ok(out)
}

pub fn download(url: &str, cap: usize, deadline: Instant) -> Result<Vec<u8>, Error> {
    let response = transport()
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

/// Rejects archive member paths whose components escape the unpack root:
/// anything containing a `..` component (absolute paths already fail the
/// root-prefix check at the join site).
fn reject_escaping(path: &str) -> Result<(), Error> {
    if path.split(['/', '\\']).any(|component| component == "..") {
        return Err(Error::Path { path: path.into() });
    }
    Ok(())
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
        reject_escaping(name)?;
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
                    reject_escaping(&override_path)?;
                    pending_override = Some(override_path);
                }
            }
            b'5' => {
                pending_override = None;
                std::fs::create_dir_all(&target).map_err(|source| Error::Io { source })?
            }
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

/// Locates one primary module by the exact trailing path components
/// (`["six.py"]`, `["idna", "core.py"]`, `["yaml", "__init__.py"]`), so
/// src-layout and package-dir sdists both resolve without guessing.
pub fn find_primary(root: &Path, suffix: &[&str]) -> Result<PathBuf, Error> {
    fn walk(path: &Path, suffix: &[&str]) -> io::Result<Option<PathBuf>> {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let matches = path
                .components()
                .rev()
                .zip(suffix.iter().rev())
                .all(|(component, wanted)| component.as_os_str() == *wanted)
                && path.components().count() >= suffix.len();
            if matches {
                return Ok(Some(path));
            }
            if path.is_dir()
                && let Some(found) = walk(&path, suffix)?
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    walk(root, suffix)
        .map_err(|source| Error::Io { source })?
        .ok_or(Error::MissingSource)
}

pub fn fresh_dir(label: &str) -> Result<PathBuf, Error> {
    // Every call gets a unique directory: a leftover directory from an
    // aborted earlier run must never be silently reused as this run's
    // journal or artifact store.
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "nudox-python-{label}-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).map_err(|source| Error::Io { source })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{Error, unpack};
    use flate2::Compression;
    use flate2::write::GzEncoder;

    /// Wraps one crafted tar stream in the gzip envelope `unpack` requires.
    fn gz(tar: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
        std::io::Write::write_all(&mut encoder, tar).expect("fixture tar writes into memory");
        encoder.finish().expect("fixture gzip finish")
    }

    /// Writes one 512-byte ustar header: `name`, octal `size`, kind.
    fn header(name: &[u8; 100], size: u32, kind: u8) -> [u8; 512] {
        let mut block = [0_u8; 512];
        block[..100].copy_from_slice(name);
        // The ustar size field is 12 bytes: 11 octal digits plus the NUL.
        let octal = format!("{size:011o}\0");
        block[124..136].copy_from_slice(octal.as_bytes());
        block[156] = kind;
        block
    }

    /// One regular-file entry with its padded payload.
    fn entry(name: &[u8; 100], payload: &[u8]) -> Vec<u8> {
        let mut bytes = header(name, payload.len() as u32, b'0').to_vec();
        let padded = payload.len().div_ceil(512) * 512;
        bytes.extend_from_slice(payload);
        bytes.resize(bytes.len() + padded - payload.len(), 0);
        bytes
    }

    /// A `..` member name must be rejected, never written outside root.
    #[test]
    fn traversal_member_is_typed_rejection() {
        let mut tar = entry(&make_name("../pwned.txt"), b"hostile");
        tar.extend_from_slice(&[0_u8; 1024]);
        let archive = gz(&tar);
        let root = std::env::temp_dir().join("nudox-traversal-root");
        std::fs::create_dir_all(&root).unwrap();
        let outcome = unpack(&archive, &root);
        assert!(matches!(outcome, Err(Error::Path { .. })));
        drop(std::fs::remove_dir_all(&root));
    }

    /// A pax `path=` record renames the next regular entry.
    #[test]
    fn pax_path_override_renames_next_entry() {
        let mut tar = header(&make_name("pax"), 26, b'x').to_vec();
        let record = b"26 path=renamed/module.py\n";
        tar.extend_from_slice(record);
        tar.resize(tar.len() + 512 - record.len(), 0);
        tar.extend_from_slice(&entry(&make_name("orig.py"), b"content"));
        tar.extend_from_slice(&[0_u8; 1024]);
        let archive = gz(&tar);
        let root = std::env::temp_dir().join("nudox-pax-root");
        std::fs::create_dir_all(&root).expect("root");
        unpack(&archive, &root).expect("unpack");
        assert!(root.join("renamed/module.py").is_file());
        assert!(!root.join("orig.py").exists());
        drop(std::fs::remove_dir_all(&root));
    }

    /// A pax `path=` override may not escape the root either.
    #[test]
    fn pax_path_override_traversal_is_typed_rejection() {
        let mut tar = header(&make_name("pax"), 31, b'x').to_vec();
        let record = b"31 path=../../escaped/pwned.py\n";
        tar.extend_from_slice(record);
        tar.resize(tar.len() + 512 - record.len(), 0);
        tar.extend_from_slice(&entry(&make_name("orig.py"), b"content"));
        tar.extend_from_slice(&[0_u8; 1024]);
        let archive = gz(&tar);
        let root = std::env::temp_dir().join("nudox-pax-escape-root");
        std::fs::create_dir_all(&root).expect("root");
        let outcome = unpack(&archive, &root);
        assert!(matches!(outcome, Err(Error::Path { .. })));
        drop(std::fs::remove_dir_all(&root));
    }

    fn make_name(name: &str) -> [u8; 100] {
        let mut bytes = [0_u8; 100];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        bytes
    }
}
