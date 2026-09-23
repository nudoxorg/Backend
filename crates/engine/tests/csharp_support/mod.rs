//! Shared transport, archive, and oracle plumbing for the real nuget-corpus
//! lifecycle tests. Mirrors the python support module's API shape: typed PURL
//! parsing, a bounded digest-verified download, a minimal ZIP unpack, and
//! fresh fixture directories — plus the vendored Roslyn oracle publisher that
//! every journey runs its authority images through.

use flate2::read::DeflateDecoder;
use sha2::{Digest, Sha256, Sha512};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};
use thiserror::Error;

/// Per-entry uncompressed-byte bound enforced from the central directory.
pub const ENTRY_BOUND: usize = 4 * 1024 * 1024;
/// Total unpacked-byte bound enforced across every entry.
pub const UNPACK_BOUND: usize = 32 * 1024 * 1024;
/// Central-entry count bound.
pub const ENTRY_COUNT_BOUND: usize = 4096;
/// Bounded readback of one produced authority image.
const IMAGE_BOUND: usize = 32 * 1024 * 1024;
/// Fixed byte width of one ZIP central-directory entry.
const CENTRAL_FIXED_BYTES: usize = 46;
/// Fixed byte width of one ZIP local file header.
const LOCAL_FIXED_BYTES: usize = 30;
/// Fixed byte width of the ZIP end-of-central-directory record.
const END_RECORD_BYTES: usize = 22;
/// Local-file-header signature.
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
/// Central-directory-entry signature.
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
/// End-of-central-directory signature.
const END_SIGNATURE: u32 = 0x0605_4b50;
/// The zip64 coordinate sentinel, always representable in `usize`.
#[allow(clippy::as_conversions, reason = "u32 always fits usize")]
const U32_SENTINEL: usize = u32::MAX as usize;

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
    #[error("malformed base64 digest: {input}")]
    Base64 { input: String },
    #[error("ZIP archive rejected: {cause}")]
    Archive { cause: ArchiveCause },
    #[error("archive member path rejected: {path}")]
    Path { path: String },
    #[error("archive capacity {bound} exceeded after observing {observed}")]
    Capacity { bound: usize, observed: usize },
    #[error("required primary source was not found")]
    MissingSource,
    #[error("fixture filesystem operation failed: {source}")]
    Io {
        #[source]
        source: io::Error,
    },
    #[error("no C# oracle toolchain: COMPILER_CSHARP_COMPILER is unset and no PATH dotnet exists")]
    Toolchain,
    #[error("oracle publish failed: {cause}")]
    OraclePublish { cause: String },
    #[error("oracle run failed: {cause}")]
    OracleRun { cause: String },
}

/// Exact structural cause of one ZIP rejection.
#[derive(Debug, Error)]
pub enum ArchiveCause {
    #[error("end-of-central-directory record missing or truncated")]
    EndRecord,
    #[error("central directory escapes the archive bytes")]
    DirectoryOutside,
    #[error("central entry header missing or carries a wrong signature")]
    EntryHeader,
    #[error("central entry claims zip64 32-bit sentinels, which this reader does not admit")]
    Zip64,
    #[error("local file header missing, truncated, or carries a wrong signature")]
    LocalHeader,
    #[error("entry data escapes the archive bytes")]
    EntryOutside,
    #[error(
        "entry data digested {observed:#010x} where the central directory declared {declared:#010x}"
    )]
    Crc { declared: u32, observed: u32 },
    #[error("entry produced {observed} bytes where the central directory declared {declared}")]
    EntrySize { declared: usize, observed: usize },
    #[error("unsupported compression method {method}")]
    Method { method: u16 },
    #[error("deflate stream rejected: {source}")]
    Inflate {
        #[source]
        source: io::Error,
    },
}

/// Cloneable terminal of the one-time oracle publish. The process-wide cache
/// shares the outcome across every journey, so its failure side must be
/// cloneable; `From<SetupFault> for Error` maps each variant exactly.
#[derive(Clone, Debug, Error)]
pub enum SetupFault {
    #[error("no C# oracle toolchain: COMPILER_CSHARP_COMPILER is unset and no PATH dotnet exists")]
    Toolchain,
    #[error("oracle publish failed: {cause}")]
    Publish { cause: String },
}

impl From<SetupFault> for Error {
    fn from(fault: SetupFault) -> Self {
        match fault {
            SetupFault::Toolchain => Error::Toolchain,
            SetupFault::Publish { cause } => Error::OraclePublish { cause },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Purl {
    pub ecosystem: String,
    pub name: String,
    pub version: String,
}

/// Parses `nuget:ID@VERSION`, rejecting any other ecosystem, a missing or
/// empty version, a double `@`, or an empty package id.
impl Purl {
    pub fn parse(input: &str) -> Result<Self, Error> {
        let fault = || Error::Purl {
            input: input.into(),
        };
        let mut parts = input.split('@');
        let left = parts.next().ok_or_else(fault)?;
        let version = parts.next().ok_or_else(fault)?;
        if parts.next().is_some() || version.is_empty() {
            return Err(fault());
        }
        let mut name = left.split(':');
        let ecosystem = name.next().ok_or_else(fault)?;
        let name = name.next().ok_or_else(fault)?;
        if ecosystem != "nuget" || name.is_empty() {
            return Err(fault());
        }
        Ok(Self {
            ecosystem: ecosystem.into(),
            name: name.into(),
            version: version.into(),
        })
    }
}

pub fn sha512(bytes: &[u8]) -> [u8; 64] {
    Sha512::digest(bytes).into()
}

/// One shared transport with a hard 30-second global timeout: ureq 3.x
/// carries timeouts on the agent config, not on individual requests.
fn transport() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent()
}

/// Locates the pinned package archive and its declared SHA-512 digest.
///
/// Returns the flat-container `.nupkg` URL plus the digest bytes the
/// registry declares for those exact bytes. The declared digest comes from
/// the `.nupkg.sha512` sidecar blob; this registry deployment answers 404
/// for sidecar blobs, so the same declared digest is then read from the
/// archive blob's own `x-ms-meta-SHA512` storage metadata, which carries the
/// identical base64 SHA-512 the sidecar used to hold. Whichever source
/// answers, the bytes travel through the same base64 decoder and the same
/// typed errors.
pub fn locate(purl: &Purl) -> Result<(String, [u8; 64]), Error> {
    let id = purl.name.to_ascii_lowercase();
    let version = purl.version.to_ascii_lowercase();
    let base = format!("https://api.nuget.org/v3-flatcontainer/{id}/{version}");
    let url = format!("{base}/{id}.{version}.nupkg");
    let sidecar = format!("{url}.sha512");
    match transport().get(&sidecar).call() {
        Ok(response) => {
            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(Error::Status { status });
            }
            let mut text = String::new();
            response
                .into_body()
                .into_reader()
                .take(4 * 1024)
                .read_to_string(&mut text)
                .map_err(|source| Error::Read {
                    observed: text.len(),
                    source,
                })?;
            Ok((url, decode_base64_sha512(&text)?))
        }
        // The registry no longer serves sidecar blobs; the declared digest
        // travels on the archive blob's own storage metadata instead.
        Err(ureq::Error::StatusCode(404)) => {
            let declared = blob_metadata_digest(&url)?;
            Ok((url, declared))
        }
        Err(source) => Err(Error::Network { source }),
    }
}

/// Reads the declared digest from the archive blob's `x-ms-meta-SHA512`
/// storage metadata after the sidecar answered 404.
fn blob_metadata_digest(url: &str) -> Result<[u8; 64], Error> {
    let response = transport()
        .head(url)
        .call()
        .map_err(|source| Error::Network { source })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Status { status });
    }
    let declared = response
        .headers()
        .get("x-ms-meta-SHA512")
        .and_then(|value| value.to_str().ok())
        .ok_or(Error::Status { status })?;
    decode_base64_sha512(declared)
}

/// Decodes exactly the base64 SHA-512 shape (88 characters ending in `==`)
/// into its 64 digest bytes.
fn decode_base64_sha512(text: &str) -> Result<[u8; 64], Error> {
    fn sextet(byte: u8, input: &str) -> Result<u32, Error> {
        let value = match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => {
                return Err(Error::Base64 {
                    input: input.into(),
                });
            }
        };
        Ok(value)
    }
    let fault = || Error::Base64 { input: text.into() };
    let bytes = text.trim().as_bytes();
    if bytes.len() != 88 || bytes[86] != b'=' || bytes[87] != b'=' {
        return Err(fault());
    }
    let mut out = [0_u8; 64];
    for (index, group) in bytes.chunks_exact(4).enumerate() {
        // The two padding characters live only in the final group, whose
        // two data sextets carry the single trailing byte.
        let padded = index == 21;
        if group[..2].iter().any(|byte| *byte == b'=') || (!padded && group[2] == b'=') {
            return Err(fault());
        }
        let word = if padded {
            sextet(group[0], text)? << 18 | sextet(group[1], text)? << 12
        } else {
            sextet(group[0], text)? << 18
                | sextet(group[1], text)? << 12
                | sextet(group[2], text)? << 6
                | sextet(group[3], text)?
        };
        let produced = if padded { 1 } else { 3 };
        for slot in 0..produced {
            let shift = 16 - 8 * slot;
            out[index * 3 + slot] = u8::try_from((word >> shift) & 0xFF).map_err(|_| fault())?;
        }
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
    let bound = u64::try_from(cap.saturating_add(1)).unwrap_or(u64::MAX);
    let mut reader = response.into_body().into_reader().take(bound);
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

/// Unpacks one ZIP archive under `root` through its central directory.
///
/// Every bound (entry count, per-entry bytes, total bytes) comes from the
/// central-directory records, never from the local headers; every written
/// path is checked against root escape before any byte is written.
pub fn unpack(bytes: &[u8], root: &Path) -> Result<(), Error> {
    let end = end_record(bytes)?;
    let total = narrow_u16(bytes, end + 10, ArchiveCause::EndRecord)?;
    let directory_size = narrow_u32(bytes, end + 12, ArchiveCause::EndRecord)?;
    let directory_at = narrow_u32(bytes, end + 16, ArchiveCause::EndRecord)?;
    if total > ENTRY_COUNT_BOUND {
        return Err(Error::Capacity {
            bound: ENTRY_COUNT_BOUND,
            observed: total,
        });
    }
    if directory_at
        .checked_add(directory_size)
        .is_none_or(|tail| tail > end)
    {
        return Err(archive(ArchiveCause::DirectoryOutside));
    }
    let mut written = 0_usize;
    let mut cursor = directory_at;
    for _ in 0..total {
        let entry = central_entry(bytes, cursor, end)?;
        cursor += CENTRAL_FIXED_BYTES + entry.name_len + entry.extra_len + entry.comment_len;
        let target = root.join(&entry.name);
        if entry.name.ends_with('/') && entry.uncompressed == 0 {
            std::fs::create_dir_all(&target).map_err(|source| Error::Io { source })?;
            continue;
        }
        if entry.uncompressed > ENTRY_BOUND {
            return Err(Error::Capacity {
                bound: ENTRY_BOUND,
                observed: entry.uncompressed,
            });
        }
        if written + entry.uncompressed > UNPACK_BOUND {
            return Err(Error::Capacity {
                bound: UNPACK_BOUND,
                observed: written + entry.uncompressed,
            });
        }
        let data = entry_data(bytes, &entry)?;
        let payload = match entry.method {
            0 => {
                if entry.compressed != entry.uncompressed {
                    return Err(Error::Archive {
                        cause: ArchiveCause::EntrySize {
                            declared: entry.uncompressed,
                            observed: entry.compressed,
                        },
                    });
                }
                data.to_vec()
            }
            8 => {
                let mut decoded = Vec::with_capacity(entry.uncompressed);
                DeflateDecoder::new(data)
                    .read_to_end(&mut decoded)
                    .map_err(|source| Error::Archive {
                        cause: ArchiveCause::Inflate { source },
                    })?;
                decoded
            }
            method => {
                return Err(Error::Archive {
                    cause: ArchiveCause::Method { method },
                });
            }
        };
        if payload.len() != entry.uncompressed {
            return Err(Error::Archive {
                cause: ArchiveCause::EntrySize {
                    declared: entry.uncompressed,
                    observed: payload.len(),
                },
            });
        }
        let digested = crc32(&payload);
        if digested != entry.crc {
            return Err(Error::Archive {
                cause: ArchiveCause::Crc {
                    declared: entry.crc,
                    observed: digested,
                },
            });
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io { source })?;
        }
        std::fs::write(&target, &payload).map_err(|source| Error::Io { source })?;
        written += payload.len();
    }
    Ok(())
}

fn archive(cause: ArchiveCause) -> Error {
    Error::Archive { cause }
}

struct CentralEntry {
    name: String,
    method: u16,
    crc: u32,
    compressed: usize,
    uncompressed: usize,
    local_offset: usize,
    name_len: usize,
    extra_len: usize,
    comment_len: usize,
}

/// Locates the end-of-central-directory record: the last 22-byte record
/// whose declared comment length covers exactly the remaining tail.
fn end_record(bytes: &[u8]) -> Result<usize, Error> {
    let Some(last) = bytes.len().checked_sub(END_RECORD_BYTES) else {
        return Err(archive(ArchiveCause::EndRecord));
    };
    let window_start = bytes
        .len()
        .saturating_sub(END_RECORD_BYTES + usize::from(u16::MAX));
    for at in (window_start..=last).rev() {
        if read_u32(bytes, at) == Some(END_SIGNATURE) {
            let comment = narrow_u16(bytes, at + 20, ArchiveCause::EndRecord)?;
            if at + END_RECORD_BYTES + comment == bytes.len() {
                return Ok(at);
            }
        }
    }
    Err(archive(ArchiveCause::EndRecord))
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let raw: [u8; 2] = bytes.get(at..at + 2)?.try_into().ok()?;
    Some(u16::from_le_bytes(raw))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

fn narrow_u16(bytes: &[u8], at: usize, cause: ArchiveCause) -> Result<usize, Error> {
    match read_u16(bytes, at) {
        Some(value) => Ok(usize::from(value)),
        None => Err(archive(cause)),
    }
}

fn narrow_u32(bytes: &[u8], at: usize, cause: ArchiveCause) -> Result<usize, Error> {
    match read_u32(bytes, at) {
        Some(value) => usize::try_from(value).map_err(|_| archive(cause)),
        None => Err(archive(cause)),
    }
}

/// The CRC-32 (IEEE) lookup table, evaluated once at compile time.
const fn crc_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut index = 0_usize;
    while index < 256 {
        // The slot index 0..256 always fits u32.
        #[allow(clippy::as_conversions, reason = "0..256 always fits u32")]
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                (value >> 1) ^ 0xEDB8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

/// CRC-32 over one entry's uncompressed payload, exactly the checksum the
/// central directory declares.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        // The masked byte always fits usize.
        #[allow(clippy::as_conversions, reason = "a masked byte always fits usize")]
        let slot = ((crc ^ u32::from(*byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC_TABLE[slot];
    }
    !crc
}

/// Parses one central-directory entry at `cursor` and validates its
/// archive-relative member path.
fn central_entry(bytes: &[u8], cursor: usize, directory_end: usize) -> Result<CentralEntry, Error> {
    let fixed = cursor
        .checked_add(CENTRAL_FIXED_BYTES)
        .ok_or_else(|| archive(ArchiveCause::EntryHeader))?;
    if fixed > directory_end || fixed > bytes.len() {
        return Err(archive(ArchiveCause::EntryHeader));
    }
    let header = &bytes[cursor..fixed];
    if read_u32(header, 0) != Some(CENTRAL_SIGNATURE) {
        return Err(archive(ArchiveCause::EntryHeader));
    }
    let fault = || archive(ArchiveCause::EntryHeader);
    let wide = |at: usize| narrow_u16(header, at, ArchiveCause::EntryHeader);
    let room = |at: usize| narrow_u32(header, at, ArchiveCause::EntryHeader);
    let name_len = wide(28)?;
    let extra_len = wide(30)?;
    let comment_len = wide(32)?;
    let method = read_u16(header, 10).ok_or_else(fault)?;
    let compressed = room(20)?;
    let uncompressed = room(24)?;
    let local_offset = room(42)?;
    // The zip64 32-bit sentinels mark an archive this minimal reader does
    // not admit; the caps below are the reader's real bounds.
    if compressed == U32_SENTINEL || uncompressed == U32_SENTINEL || local_offset == U32_SENTINEL {
        return Err(archive(ArchiveCause::Zip64));
    }
    let entry = CentralEntry {
        name: String::new(),
        method,
        crc: read_u32(header, 16).ok_or_else(fault)?,
        compressed,
        uncompressed,
        local_offset,
        name_len,
        extra_len,
        comment_len,
    };
    let tail = fixed
        .checked_add(entry.name_len + entry.extra_len + entry.comment_len)
        .ok_or_else(fault)?;
    if tail > directory_end || tail > bytes.len() {
        return Err(archive(ArchiveCause::EntryHeader));
    }
    let raw = &bytes[fixed..fixed + entry.name_len];
    let name = String::from_utf8_lossy(raw).into_owned();
    let rejected = name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.split('/').any(|component| component == "..")
        || name
            .split('/')
            .next()
            .is_some_and(|first| first.contains(':'));
    if rejected {
        return Err(Error::Path { path: name });
    }
    Ok(CentralEntry { name, ..entry })
}

/// Slices one entry's compressed payload through the local header's own
/// name/extra lengths; every size bound comes from the central record.
fn entry_data<'bytes>(bytes: &'bytes [u8], entry: &CentralEntry) -> Result<&'bytes [u8], Error> {
    let header = bytes
        .get(entry.local_offset..entry.local_offset + LOCAL_FIXED_BYTES)
        .ok_or_else(|| archive(ArchiveCause::LocalHeader))?;
    if read_u32(header, 0) != Some(LOCAL_SIGNATURE) {
        return Err(archive(ArchiveCause::LocalHeader));
    }
    let fault = || archive(ArchiveCause::LocalHeader);
    let local_name = usize::from(read_u16(header, 26).ok_or_else(fault)?);
    let local_extra = usize::from(read_u16(header, 28).ok_or_else(fault)?);
    let start = entry
        .local_offset
        .checked_add(LOCAL_FIXED_BYTES + local_name + local_extra)
        .ok_or_else(|| archive(ArchiveCause::EntryOutside))?;
    bytes
        .get(start..start + entry.compressed)
        .ok_or_else(|| archive(ArchiveCause::EntryOutside))
}

/// Locates the configured dotnet executable: `COMPILER_CSHARP_COMPILER`
/// first, then a PATH `dotnet`; a missing tool is a typed terminal, never a
/// skip.
pub fn dotnet_executable() -> Result<PathBuf, Error> {
    if let Some(configured) = std::env::var_os("COMPILER_CSHARP_COMPILER") {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return path.canonicalize().map_err(|source| Error::Io { source });
        }
        return Err(Error::Toolchain);
    }
    std::env::var_os("PATH")
        .as_deref()
        .and_then(|paths| {
            std::env::split_paths(paths)
                .map(|dir| dir.join("dotnet"))
                .find(|path| path.is_file())
        })
        .ok_or(Error::Toolchain)
}

/// The vendored Roslyn oracle helper checked into the repository.
fn helper_dir() -> Result<PathBuf, Error> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| Error::OraclePublish {
        cause: "CARGO_MANIFEST_DIR is unset".into(),
    })?;
    Ok(Path::new(&manifest).join("../../frontends/csharp/src/legacy/helper"))
}

/// Publishes the vendored oracle once per process into a fresh temporary
/// directory and returns the published `oracle.dll` path.
fn publish_oracle() -> Result<PathBuf, SetupFault> {
    let fault = |cause: String| SetupFault::Publish { cause };
    let dotnet = dotnet_executable().map_err(|_| SetupFault::Toolchain)?;
    let helper = helper_dir().map_err(|error| fault(error.to_string()))?;
    let out = fresh_dir("oracle-publish").map_err(|error| fault(error.to_string()))?;
    // Every concurrently running test process publishes the same checked-in
    // helper project. `dotnet publish` writes its intermediate build state
    // (`obj/`, `BaseIntermediateOutputPath`) AND its own build output
    // (`bin/`, `BaseOutputPath`) under the project directory by default —
    // `-o` only redirects the final publish copy, not that intermediate
    // build step — so without explicit, per-process overrides for both, all
    // processes race on the same `obj/`/`bin/` files and MSBuild fails
    // nondeterministically. Give each publish its own intermediate and
    // build-output directories alongside its own publish directory so
    // concurrent runs never touch the same files.
    let intermediate = out.join("obj");
    let mut intermediate_arg = std::ffi::OsString::from("-p:BaseIntermediateOutputPath=");
    intermediate_arg.push(&intermediate);
    intermediate_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let output_base = out.join("bin");
    let mut output_base_arg = std::ffi::OsString::from("-p:BaseOutputPath=");
    output_base_arg.push(&output_base);
    output_base_arg.push(std::path::MAIN_SEPARATOR.to_string());
    // `UseSharedCompilation=false`: see csharp_packaging.rs's build
    // invocation for why every concurrent build of this shared checked-in
    // project must not share MSBuild's ambient VBCSCompiler node.
    let output = std::process::Command::new(dotnet)
        .args([
            "publish",
            "oracle.csproj",
            "-c",
            "Release",
            "--nologo",
            "-p:UseSharedCompilation=false",
        ])
        .arg(&intermediate_arg)
        .arg(&output_base_arg)
        .arg("-o")
        .arg(&out)
        .current_dir(&helper)
        .output()
        .map_err(|source| fault(format!("dotnet publish spawn failed: {source}")))?;
    if !output.status.success() {
        // `dotnet publish` writes build/compile errors to stdout, not
        // stderr (stderr is reserved for host/CLI-level failures). Reading
        // only `stderr` silently discarded the real cause and reported an
        // empty string. Surface both streams so the failure is diagnosable.
        return Err(fault(format!(
            "dotnet publish failed: stdout: {} stderr: {}",
            excerpt(&output.stdout),
            excerpt(&output.stderr)
        )));
    }
    Ok(out.join("oracle.dll"))
}

/// The one oracle publish per process: every journey reuses the published
/// `oracle.dll` from the process-wide cache.
pub fn published_oracle() -> Result<&'static Path, Error> {
    static PUBLISHED: OnceLock<Result<PathBuf, SetupFault>> = OnceLock::new();
    PUBLISHED
        .get_or_init(publish_oracle)
        .as_ref()
        .map(|path| path.as_path())
        .map_err(|fault| Error::from(fault.clone()))
}

/// Caps one diagnostic excerpt so a failing oracle cannot flood the test log.
fn excerpt(bytes: &[u8]) -> String {
    let cut = bytes.len().min(2048);
    String::from_utf8_lossy(&bytes[..cut]).into_owned()
}

/// Runs the published Roslyn oracle once for one bound source file and
/// returns the produced authority image bytes.
///
/// `roots` are the compiled source directories, `binding` is the exact
/// source file whose raw bytes the image digests, and the run is bounded by
/// `deadline`: a run that outlives it is killed and rejected typed.
pub fn authority_image(
    roots: &[&Path],
    assembly: &str,
    binding: &Path,
    deadline: Instant,
) -> Result<Vec<u8>, Error> {
    let oracle = published_oracle()?;
    let dotnet = dotnet_executable()?;
    let out = fresh_dir("authority-image")?.join("oracle.img");
    let mut command = std::process::Command::new(dotnet);
    command.arg("exec").arg(oracle);
    command
        .arg("--mode")
        .arg("source")
        .arg("--assembly-name")
        .arg(assembly)
        .arg("--authority-image")
        .arg("--source-binding")
        .arg(binding)
        .arg("--out")
        .arg(&out);
    for root in roots {
        command.arg("--root").arg(root);
    }
    let mut child = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|source| Error::OracleRun {
            cause: format!("oracle spawn failed: {source}"),
        })?;
    let failure = loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::OracleRun {
                cause: "oracle run exceeded its deadline".into(),
            });
        }
        match child.try_wait().map_err(|source| Error::OracleRun {
            cause: format!("oracle wait failed: {source}"),
        })? {
            Some(status) => break status,
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    if !failure.success() {
        let mut stderr = Vec::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_end(&mut stderr);
        }
        return Err(Error::OracleRun {
            cause: format!("oracle exit {failure}: {}", excerpt(&stderr)),
        });
    }
    let file = std::fs::File::open(&out).map_err(|source| Error::OracleRun {
        cause: format!("oracle wrote no authority image: {source}"),
    })?;
    let mut image = Vec::new();
    file.take(u64::try_from(IMAGE_BOUND).unwrap_or(u64::MAX))
        .read_to_end(&mut image)
        .map_err(|source| Error::Read {
            observed: image.len(),
            source,
        })?;
    Ok(image)
}

/// Creates one uniquely-named temporary fixture directory; a leftover
/// directory from an aborted earlier run must never be silently reused as
/// this run's journal, artifact store, or unpack root.
pub fn fresh_dir(label: &str) -> Result<PathBuf, Error> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "nudox-csharp-{label}-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).map_err(|source| Error::Io { source })?;
    Ok(path)
}

/// Resolves the pinned primary source by its exact archive-relative path.
pub fn primary(root: &Path, relative: &str) -> Result<PathBuf, Error> {
    let path = root.join(relative);
    if path.is_file() {
        Ok(path)
    } else {
        Err(Error::MissingSource)
    }
}

/// v4 Roslyn authority image header/directory geometry (mirrors
/// `frontends/csharp/src/legacy/image.rs`'s private layout constants; kept
/// in lockstep with that file's doc comment for the wire format).
mod image_geometry {
    pub const HEADER_BYTES: usize = 256;
    pub const DIRECTORY_OFFSET: usize = 48;
    pub const DIRECTORY_ENTRY_BYTES: usize = 16;
    pub const SECTION_COUNT: usize = 11;
    pub const DIGEST_DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v4\0";
}

/// Strips the machine-specific absolute-path prefix that the Roslyn oracle
/// bakes into every atom naming the bound source file, leaving only the
/// portion from `marker` onward.
///
/// The oracle always resolves `--root`/`--source-binding` through
/// `Path.GetFullPath` before binding `SyntaxTree.FilePath`
/// (`SourceLoader.CollectSourceFiles`), and `AuthorityImage` embeds that
/// exact `tree.FilePath` verbatim into the atom table
/// (`Atom(tree.FilePath)` in `AuthorityImage.cs`). That is deliberate for
/// the real production pipeline (`CSharpAuthorityProducer::authority_image`
/// canonicalizes and passes real absolute paths too, and downstream
/// provenance is meant to carry them), but it means a byte-exact checked-in
/// golden can only ever match a regeneration performed from the *exact same
/// absolute checkout path* it was captured from. This repository runs many
/// concurrent git worktrees at different absolute paths (`backend`,
/// `backend-fix-native`, `backend-fix-csharp`, ...), so no single committed
/// absolute path is ever universally reproducible; the committed
/// `fidelity.ncaimg`/`unicode.ncaimg` fixtures were captured from a
/// `backend-fix-csharp` worktree and therefore mismatch a byte-exact regen
/// from any other checkout, including the one the gate itself builds from.
/// Normalizing both sides to the portion of the path from `marker` onward
/// (a fixed, checkout-independent relative form) restores a comparison that
/// actually verifies the extraction is byte-reproducible, without weakening
/// it: everything other than the checkout-specific path prefix — every
/// declaration, span, type, reference and doc row — is still compared
/// byte-for-byte.
pub fn normalize_authority_image_paths(bytes: &[u8], marker: &str) -> Vec<u8> {
    use image_geometry::{DIGEST_DOMAIN, DIRECTORY_ENTRY_BYTES, DIRECTORY_OFFSET, HEADER_BYTES};

    struct DirEntry {
        tag: u16,
        row_bytes: u16,
        count: u32,
        offset: u32,
        byte_count: u32,
    }

    let u16_at = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);

    let entries: Vec<DirEntry> = (0..image_geometry::SECTION_COUNT)
        .map(|index| {
            let at = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
            DirEntry {
                tag: u16_at(bytes, at),
                row_bytes: u16_at(bytes, at + 2),
                count: u32_at(bytes, at + 4),
                offset: u32_at(bytes, at + 8),
                byte_count: u32_at(bytes, at + 12),
            }
        })
        .collect();

    // Directory index 0 is `Section::Atoms` (offset/length pairs), index 1
    // is `Section::AtomBytes` (the concatenated UTF-8 backing bytes). Every
    // other section references an atom by table *index*, never by raw byte
    // offset, so rewriting only these first two sections' content keeps
    // every later section's bytes valid unchanged.
    let atoms = &entries[0];
    let atom_bytes_section = &entries[1];
    let atoms_off = atoms.offset as usize;
    let atom_bytes_off = atom_bytes_section.offset as usize;

    let mut new_atom_bytes: Vec<u8> = Vec::new();
    let mut new_atom_rows: Vec<u8> = Vec::with_capacity(atoms.count as usize * 8);
    for index in 0..atoms.count as usize {
        let row_at = atoms_off + index * 8;
        let off = u32_at(bytes, row_at) as usize;
        let len = u32_at(bytes, row_at + 4) as usize;
        let raw = &bytes[atom_bytes_off + off..atom_bytes_off + off + len];
        let rewritten = match std::str::from_utf8(raw).ok().and_then(|s| s.find(marker)) {
            Some(at) => raw[at..].to_vec(),
            None => raw.to_vec(),
        };
        let new_off = u32::try_from(new_atom_bytes.len()).expect("image atom bytes fit u32");
        let new_len = u32::try_from(rewritten.len()).expect("image atom fits u32");
        new_atom_bytes.extend_from_slice(&rewritten);
        new_atom_rows.extend_from_slice(&new_off.to_le_bytes());
        new_atom_rows.extend_from_slice(&new_len.to_le_bytes());
    }

    let mut body = Vec::new();
    body.extend_from_slice(&new_atom_rows);
    body.extend_from_slice(&new_atom_bytes);
    for entry in &entries[2..] {
        let start = entry.offset as usize;
        let len = entry.byte_count as usize;
        body.extend_from_slice(&bytes[start..start + len]);
    }

    let mut out = vec![0u8; HEADER_BYTES];
    out.copy_from_slice(&bytes[..HEADER_BYTES]);
    let total_len = u32::try_from(HEADER_BYTES + body.len()).expect("image total fits u32");
    out[8..12].copy_from_slice(&total_len.to_le_bytes());

    let mut cursor = u32::try_from(HEADER_BYTES).expect("header fits u32");
    for (index, entry) in entries.iter().enumerate() {
        let at = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
        let byte_count = if index == 1 {
            u32::try_from(new_atom_bytes.len()).expect("atom bytes fit u32")
        } else {
            entry.byte_count
        };
        out[at..at + 2].copy_from_slice(&entry.tag.to_le_bytes());
        out[at + 2..at + 4].copy_from_slice(&entry.row_bytes.to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&entry.count.to_le_bytes());
        out[at + 8..at + 12].copy_from_slice(&cursor.to_le_bytes());
        out[at + 12..at + 16].copy_from_slice(&byte_count.to_le_bytes());
        cursor += byte_count;
    }
    out.extend_from_slice(&body);

    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(&out[..224]);
    hasher.update(&out[256..]);
    let digest = hasher.finalize();
    out[224..256].copy_from_slice(&digest);
    out
}

/// Copies one directory tree recursively, preserving file contents. Used to
/// stage the second-generation probe copy of an unpacked package.
pub fn copy_dir(source: &Path, destination: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(destination).map_err(|source| Error::Io { source })?;
    for entry in std::fs::read_dir(source).map_err(|source| Error::Io { source })? {
        let entry = entry.map_err(|source| Error::Io { source })?;
        let kind = entry.file_type().map_err(|source| Error::Io { source })?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|source| Error::Io { source })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ArchiveCause, CENTRAL_SIGNATURE, END_SIGNATURE, Error, LOCAL_SIGNATURE, crc32, unpack,
    };
    use flate2::{Compression, write::DeflateEncoder};
    use std::io::Write as _;

    /// One crafted ZIP entry under construction.
    struct FixtureEntry {
        name: String,
        payload: Vec<u8>,
        method: u16,
    }

    fn entry(name: &str, payload: &[u8], method: u16) -> FixtureEntry {
        FixtureEntry {
            name: name.into(),
            payload: payload.into(),
            method,
        }
    }

    /// Encodes entries into a complete ZIP archive: local headers + data,
    /// central directory, and end record, using the real 30-byte local and
    /// 46-byte central header layouts. Stored payloads use method 0,
    /// deflated ones method 8 via the real encoder.
    fn zip(entries: &[FixtureEntry]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for item in entries {
            let compressed = match item.method {
                0 => item.payload.clone(),
                _ => {
                    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(6));
                    encoder.write_all(&item.payload).expect("fixture deflate");
                    encoder.finish().expect("fixture deflate finish")
                }
            };
            let checksum = crc32(&item.payload);
            let local_at = out.len();
            // Local file header: signature, versions, flags, method, time,
            // date, crc, sizes, name/extra lengths.
            out.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
            out.extend_from_slice(&20_u16.to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes());
            out.extend_from_slice(&item.method.to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes());
            out.extend_from_slice(&checksum.to_le_bytes());
            out.extend_from_slice(
                &u32::try_from(compressed.len())
                    .expect("fixture size")
                    .to_le_bytes(),
            );
            out.extend_from_slice(
                &u32::try_from(item.payload.len())
                    .expect("fixture size")
                    .to_le_bytes(),
            );
            out.extend_from_slice(
                &u16::try_from(item.name.len())
                    .expect("fixture size")
                    .to_le_bytes(),
            );
            out.extend_from_slice(&0_u16.to_le_bytes());
            out.extend_from_slice(item.name.as_bytes());
            out.extend_from_slice(&compressed);
            // Central directory entry: signature, versions, flags, method,
            // times, crc, sizes, lengths, disk, attributes, local offset.
            central.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
            central.extend_from_slice(&20_u16.to_le_bytes());
            central.extend_from_slice(&20_u16.to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes());
            central.extend_from_slice(&item.method.to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes());
            central.extend_from_slice(&checksum.to_le_bytes());
            central.extend_from_slice(
                &u32::try_from(compressed.len())
                    .expect("fixture size")
                    .to_le_bytes(),
            );
            central.extend_from_slice(
                &u32::try_from(item.payload.len())
                    .expect("fixture size")
                    .to_le_bytes(),
            );
            central.extend_from_slice(
                &u16::try_from(item.name.len())
                    .expect("fixture size")
                    .to_le_bytes(),
            );
            central.extend_from_slice(&0_u16.to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes());
            central.extend_from_slice(&0_u32.to_le_bytes());
            central
                .extend_from_slice(&u32::try_from(local_at).expect("fixture size").to_le_bytes());
            central.extend_from_slice(item.name.as_bytes());
        }
        let central_at = out.len();
        out.extend_from_slice(&central);
        out.extend_from_slice(&END_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(
            &u16::try_from(entries.len())
                .expect("fixture size")
                .to_le_bytes(),
        );
        out.extend_from_slice(
            &u16::try_from(entries.len())
                .expect("fixture size")
                .to_le_bytes(),
        );
        out.extend_from_slice(
            &u32::try_from(central.len())
                .expect("fixture size")
                .to_le_bytes(),
        );
        out.extend_from_slice(
            &u32::try_from(central_at)
                .expect("fixture size")
                .to_le_bytes(),
        );
        out.extend_from_slice(&0_u16.to_le_bytes());
        out
    }

    /// Declares a fixed total-entries cell without touching entry count 0.
    fn with_total(archive: &[u8], total: u16) -> Vec<u8> {
        let end = archive.len() - 22;
        let mut mutated = archive.to_vec();
        mutated[end + 10..end + 12].copy_from_slice(&total.to_le_bytes());
        mutated
    }

    fn fresh(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("nudox-csharp-zip-{label}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path).expect("fixture root");
        path
    }

    /// Stored and deflated entries both round-trip through the central
    /// directory reader.
    #[test]
    fn stored_and_deflated_entries_round_trip() {
        let archive = zip(&[
            entry("lib/stored.txt", b"plain bytes", 0),
            entry("lib/deflated.txt", &b"compressed bytes ".repeat(64), 8),
        ]);
        let root = fresh("roundtrip");
        unpack(&archive, &root).expect("unpack");
        assert_eq!(
            std::fs::read(root.join("lib/stored.txt")).expect("stored"),
            b"plain bytes"
        );
        assert_eq!(
            std::fs::read(root.join("lib/deflated.txt")).expect("deflated"),
            b"compressed bytes ".repeat(64)
        );
        drop(std::fs::remove_dir_all(&root));
    }

    /// A `..` member name must be rejected, never written outside root.
    #[test]
    fn parent_traversing_entry_is_typed_rejection() {
        let archive = zip(&[entry("../pwned.cs", b"hostile", 0)]);
        let root = fresh("traversal");
        match unpack(&archive, &root) {
            Err(Error::Path { path }) => assert_eq!(path, "../pwned.cs"),
            _ => panic!("traversal admitted"),
        }
        drop(std::fs::remove_dir_all(&root));
    }

    /// An absolute member name must be rejected before any write.
    #[test]
    fn absolute_entry_is_typed_rejection() {
        let archive = zip(&[entry("/etc/pwned.cs", b"hostile", 0)]);
        let root = fresh("absolute");
        match unpack(&archive, &root) {
            Err(Error::Path { path }) => assert_eq!(path, "/etc/pwned.cs"),
            _ => panic!("absolute path admitted"),
        }
        drop(std::fs::remove_dir_all(&root));
    }

    /// A corrupted end-of-central-directory signature is a typed Archive
    /// rejection, never a partial unpack.
    #[test]
    fn corrupt_end_record_is_typed_rejection() {
        let mut archive = zip(&[entry("a.cs", b"content", 0)]);
        let at = archive.len() - 22;
        archive[at] ^= 1;
        let root = fresh("eocd");
        match unpack(&archive, &root) {
            Err(Error::Archive { cause }) => {
                assert!(matches!(cause, ArchiveCause::EndRecord))
            }
            _ => panic!("corrupt end record admitted"),
        }
        drop(std::fs::remove_dir_all(&root));
    }

    /// An entry count above the central cap is a typed capacity rejection.
    #[test]
    fn entry_count_cap_is_typed_rejection() {
        let archive = zip(&[entry("a.cs", b"content", 0)]);
        let mutated = with_total(&archive, 4097);
        let root = fresh("count");
        match unpack(&mutated, &root) {
            Err(Error::Capacity { bound, observed }) => {
                assert_eq!((bound, observed), (4096, 4097))
            }
            _ => panic!("entry count cap admitted"),
        }
        drop(std::fs::remove_dir_all(&root));
    }

    /// A per-entry uncompressed size above the cap is a typed capacity
    /// rejection observed from the central record.
    #[test]
    fn entry_size_cap_is_typed_rejection() {
        let mut archive = zip(&[entry("big.cs", b"tiny", 0)]);
        // The central entry's uncompressed-size cell sits at header offset 24;
        // claim one byte above the 4 MiB entry bound.
        let central_at = archive.len() - 22 - (46 + "big.cs".len());
        let claimed = (4 * 1024 * 1024 + 1) as u32;
        archive[central_at + 24..central_at + 28].copy_from_slice(&claimed.to_le_bytes());
        let root = fresh("size");
        match unpack(&archive, &root) {
            Err(Error::Capacity { bound, observed }) => {
                assert_eq!((bound, observed), (4 * 1024 * 1024, 4 * 1024 * 1024 + 1))
            }
            _ => panic!("entry size cap admitted"),
        }
        drop(std::fs::remove_dir_all(&root));
    }

    /// A deflated entry whose stream is corrupted is a typed Archive
    /// rejection carrying the inflate cause.
    #[test]
    fn corrupted_deflate_stream_is_typed_rejection() {
        let mut archive = zip(&[entry("lib/text.cs", &b"payload ".repeat(128), 8)]);
        let data_at = 30 + "lib/text.cs".len();
        archive[data_at] ^= 0xFF;
        let root = fresh("inflate");
        match unpack(&archive, &root) {
            Err(Error::Archive { cause }) => {
                assert!(matches!(cause, ArchiveCause::Inflate { .. }))
            }
            _ => panic!("corrupted deflate stream admitted"),
        }
        drop(std::fs::remove_dir_all(&root));
    }
}
