use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{io::{self, Read}, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{Duration, Instant}};
use thiserror::Error;

/// Maximum bytes retained from one registry metadata response.
pub const METADATA_CAP: usize = 4 * 1024 * 1024;
/// Maximum compressed bytes retained from one registry archive.
pub const ARCHIVE_CAP: usize = 32 * 1024 * 1024;
/// Maximum total extracted bytes admitted from one archive.
pub const UNPACKED_CAP: usize = 128 * 1024 * 1024;
/// date-fns publishes about 4,100 files; declaration-only extraction avoids the one-file budget overrun while retaining tar-bomb bounds.
pub const DECLARATION_FILE_EXTENSIONS: [&str; 3] = [".d.ts", ".ts", "package.json"];
/// Global transport deadline, including response body consumption.
pub const NETWORK_DEADLINE: Duration = Duration::from_secs(30);
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum Error {
    #[error("malformed npm PURL: {input}")] Purl { input: String },
    #[error("network request failed: {source}")] Network { #[source] source: ureq::Error },
    #[error("HTTP status {status}")] Status { status: u16 },
    #[error("response exceeded {cap} bytes after observing {observed}")] Cap { cap: usize, observed: usize },
    #[error("deadline expired after observing {observed} bytes")] Deadline { observed: usize },
    #[error("response read failed after observing {observed} bytes: {source}")] Read { observed: usize, #[source] source: io::Error },
    #[error("archive rejected: {source}")] Archive { #[source] source: io::Error },
    #[error("archive entry type {kind} is not permitted")] Entry { kind: u8 },
    #[error("archive path rejected: {path}")] Path { path: String },
    #[error("package.json is missing or invalid: {source}")] Package { #[source] source: serde_json::Error },
    #[error("package has no declaration entry; searched {searched:?}")] MissingTypes { searched: Vec<String> },
    #[error("package directory was not found: {name}")] MissingPackage { name: String },
    #[error("fixture filesystem operation failed: {source}")] Io { #[source] source: io::Error },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Purl { pub name: String, pub version: String }
impl Purl {
    pub fn parse(input: &str) -> Result<Self, Error> {
        let rest = input.strip_prefix("npm:").ok_or_else(|| Error::Purl { input: input.into() })?;
        let at = rest.rfind('@').ok_or_else(|| Error::Purl { input: input.into() })?;
        let (name, version) = rest.split_at(at);
        let version = &version[1..];
        let valid_name = !name.is_empty() && (!name.starts_with('@') || name[1..].contains('/'));
        if !valid_name || version.is_empty() || version.contains('@') { return Err(Error::Purl { input: input.into() }); }
        Ok(Self { name: name.into(), version: version.into() })
    }
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] { Sha256::digest(bytes).into() }
pub fn hex(bytes: &[u8; 32]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

fn transport() -> ureq::Agent { ureq::Agent::config_builder().timeout_global(Some(NETWORK_DEADLINE)).build().new_agent() }

pub fn locate(purl: &Purl) -> Result<String, Error> {
    let (directory, base) = match purl.name.rsplit_once('/') {
        Some((scope, name)) if scope.starts_with('@') => (format!("{scope}/{name}"), name.to_owned()),
        _ => (purl.name.clone(), purl.name.clone()),
    };
    Ok(format!("https://registry.npmjs.org/{directory}/-/{base}-{}.tgz", purl.version))
}

pub fn download(url: &str, cap: usize, deadline: Instant) -> Result<Vec<u8>, Error> {
    let response = transport().get(url).call().map_err(|source| Error::Network { source })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) { return Err(Error::Status { status }); }
    let mut reader = response.into_body().into_reader().take(cap.saturating_add(1) as u64);
    let mut bytes = Vec::new();
    loop {
        if Instant::now() >= deadline { return Err(Error::Deadline { observed: bytes.len() }); }
        let mut chunk = [0_u8; 8192];
        let read = reader.read(&mut chunk).map_err(|source| Error::Read { observed: bytes.len(), source })?;
        if read == 0 { return Ok(bytes); }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > cap { return Err(Error::Cap { cap, observed: bytes.len() }); }
    }
}

fn safe(path: &str) -> Result<(), Error> {
    if path.starts_with('/') || path.split('/').any(|part| part == "..") { return Err(Error::Path { path: path.into() }); }
    Ok(())
}

pub fn unpack(bytes: &[u8], root: &Path) -> Result<(), Error> {
    let mut decoder = GzDecoder::new(bytes); let mut tar = Vec::new();
    decoder.read_to_end(&mut tar).map_err(|source| Error::Archive { source })?;
    let mut offset = 0; let mut total: usize = 0;
    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512]; if header.iter().all(|b| *b == 0) { break; }
        let end = header[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&header[..end]).map_err(|_| Error::Path { path: "non-utf8".into() })?;
        safe(name)?;
        let size_text = std::str::from_utf8(&header[124..136]).map_err(|_| Error::Path { path: name.into() })?;
        let size = usize::from_str_radix(size_text.trim_matches('\0').trim(), 8).map_err(|_| Error::Path { path: name.into() })?;
        total = total.checked_add(size).ok_or_else(|| Error::Cap { cap: UNPACKED_CAP, observed: usize::MAX })?;
        if total > UNPACKED_CAP { return Err(Error::Cap { cap: UNPACKED_CAP, observed: total }); }
        let payload = tar.get(offset + 512..offset + 512 + size).ok_or_else(|| Error::Path { path: name.into() })?;
        let destination = root.join(name.trim_start_matches("package/"));
        if !destination.starts_with(root) { return Err(Error::Path { path: name.into() }); }
        match header[156] { 0 | b'0' => { if let Some(parent) = destination.parent() { std::fs::create_dir_all(parent).map_err(|source| Error::Io { source })?; } std::fs::write(destination, payload).map_err(|source| Error::Io { source })?; }, b'5' => std::fs::create_dir_all(destination).map_err(|source| Error::Io { source })?, kind => return Err(Error::Entry { kind }) }
        offset += 512 + size.div_ceil(512) * 512;
    }
    Ok(())
}

pub fn unpack_declarations(bytes: &[u8], root: &Path) -> Result<(), Error> {
    let mut decoder = GzDecoder::new(bytes); let mut tar = Vec::new();
    decoder.read_to_end(&mut tar).map_err(|source| Error::Archive { source })?;
    let mut offset = 0; let mut total: usize = 0;
    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512]; if header.iter().all(|b| *b == 0) { break; }
        let end = header[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&header[..end]).map_err(|_| Error::Path { path: "non-utf8".into() })?; safe(name)?;
        let size_text = std::str::from_utf8(&header[124..136]).map_err(|_| Error::Path { path: name.into() })?;
        let size = usize::from_str_radix(size_text.trim_matches('\0').trim(), 8).map_err(|_| Error::Path { path: name.into() })?;
        total = total.checked_add(size).ok_or_else(|| Error::Cap { cap: UNPACKED_CAP, observed: usize::MAX })?;
        if total > UNPACKED_CAP { return Err(Error::Cap { cap: UNPACKED_CAP, observed: total }); }
        let payload = tar.get(offset + 512..offset + 512 + size).ok_or_else(|| Error::Path { path: name.into() })?;
        let relative = name.trim_start_matches("package/");
        if header[156] == b'5' || DECLARATION_FILE_EXTENSIONS.iter().any(|extension| relative.ends_with(extension)) {
            let destination = root.join(relative); if !destination.starts_with(root) { return Err(Error::Path { path: name.into() }); }
            match header[156] { 0 | b'0' => { if let Some(parent) = destination.parent() { std::fs::create_dir_all(parent).map_err(|source| Error::Io { source })?; } std::fs::write(destination, payload).map_err(|source| Error::Io { source })?; }, b'5' => std::fs::create_dir_all(destination).map_err(|source| Error::Io { source })?, kind => return Err(Error::Entry { kind }) }
        }
        offset += 512 + size.div_ceil(512) * 512;
    }
    Ok(())
}

pub fn package_root(root: &Path, name: &str) -> Result<PathBuf, Error> {
    let wanted = format!("\"name\":\"{name}\"");
    fn walk(path: &Path, wanted: &str) -> io::Result<Option<PathBuf>> {
        for item in std::fs::read_dir(path)? { let item = item?; let p = item.path(); if p.file_name().is_some_and(|n| n == "package.json") { let text = std::fs::read_to_string(&p)?; if text.replace([' ', '\n', '\r', '\t'], "").contains(wanted) { return Ok(p.parent().map(Path::to_path_buf)); } } if p.is_dir() && let Some(found) = walk(&p, wanted)? { return Ok(Some(found)); } }
        Ok(None)
    }
    walk(root, &wanted).map_err(|source| Error::Io { source })?.ok_or_else(|| Error::MissingPackage { name: name.into() })
}

pub fn entry(root: &Path) -> Result<PathBuf, Error> {
    let package = root.join("package.json");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&package).map_err(|source| Error::Io { source })?).map_err(|source| Error::Package { source })?;
    let mut searched = Vec::new();
    for key in ["types", "typings"] { if let Some(path) = value.get(key).and_then(serde_json::Value::as_str) { let candidate = root.join(path); searched.push(candidate.display().to_string()); if candidate.is_file() && candidate.extension().is_some_and(|extension| extension == "ts") { return Ok(candidate); } } }
    if let Some(types) = value.get("typesVersions") { searched.push(format!("typesVersions:{types}")); }
    let candidate = root.join("index.d.ts"); searched.push(candidate.display().to_string()); if candidate.is_file() { return Ok(candidate); }
    Err(Error::MissingTypes { searched })
}

pub fn sibling_types(purl: &Purl) -> Purl {
    let name = match purl.name.strip_prefix('@').and_then(|name| name.split_once('/')) { Some((scope, name)) => format!("@types/{scope}__{name}"), None => format!("@types/{}", purl.name) };
    let version = match purl.name.as_str() { "lodash" => "4.17.16", "react" => "18.3.12", "express" => "4.17.21", _ => "latest" };
    Purl { name, version: version.into() }
}

pub fn fresh_dir(label: &str) -> Result<PathBuf, Error> { let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed); let path = std::env::temp_dir().join(format!("nudox-typescript-{label}-{}-{sequence}", std::process::id())); std::fs::create_dir_all(&path).map_err(|source| Error::Io { source })?; Ok(path) }
