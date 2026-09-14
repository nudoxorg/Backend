use flate2::{Compression, write::DeflateEncoder};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("fixture filesystem operation at {path:?} failed: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("deflate fixture failed: {source}")]
    Deflate {
        #[source]
        source: io::Error,
    },
}
fn io(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_owned(),
        source,
    }
}

pub const FIRST: &[(&str, &[u8])] = &[
    (
        "Annotated.java",
        br#"/** {@link Record} */
@Deprecated
public class Annotated {
  @Deprecated public void run() throws java.io.IOException {}
  public void run(int value) {}
}
"#,
    ),
    (
        "Pair.java",
        br#"public record Pair(String left, int right) {}
"#,
    ),
];

pub const CENTRAL_NAMES: [&str; 5] = [
    "org/apache/commons/lang3/tuple/Pair.java",
    "org/apache/commons/lang3/mutable/MutableInt.java",
    "org/apache/commons/lang3/arch/Processor.java",
    "org/apache/commons/lang3/ArchUtils.java",
    "org/apache/commons/lang3/function/Consumers.java",
];

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct TempDir {
    pub path: PathBuf,
}
impl TempDir {
    pub fn new(label: &str) -> Result<Self, Error> {
        let path = std::env::temp_dir().join(format!(
            "nudox-java-lifecycle-{}-{}-{}-{label}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| io(Path::new("clock"), io::Error::other(e)))?
                .as_nanos()
        ));
        fs::create_dir(&path).map_err(|e| io(&path, e))?;
        Ok(Self { path })
    }
    pub fn remove(self) -> Result<(), Error> {
        fs::remove_dir_all(&self.path).map_err(|e| io(&self.path, e))
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    }
    fs::write(path, bytes).map_err(|e| io(path, e))
}

pub fn jar(entries: &[(&str, &[u8], bool)]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for &(name, data, deflated) in entries {
        let raw = if deflated {
            let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
            e.write_all(data)
                .map_err(|source| Error::Deflate { source })?;
            e.finish().map_err(|source| Error::Deflate { source })?
        } else {
            data.to_vec()
        };
        let offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let crc = crc32(data);
        local(
            &mut out,
            name.as_bytes(),
            if deflated { 8 } else { 0 },
            crc,
            &raw,
            data.len(),
        );
        central_record(
            &mut central,
            name.as_bytes(),
            if deflated { 8 } else { 0 },
            crc,
            &raw,
            data.len(),
            offset,
        );
    }
    let directory = u32::try_from(out.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&central);
    let count = u16::try_from(entries.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&0x06054b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(central.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(&directory.to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    Ok(out)
}
fn local(out: &mut Vec<u8>, name: &[u8], method: u16, crc: u32, data: &[u8], size: usize) {
    out.extend_from_slice(&0x04034b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&method.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&u32::try_from(size).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&(u16::try_from(name.len()).unwrap_or(u16::MAX)).to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out.extend_from_slice(name);
    out.extend_from_slice(data);
}
fn central_record(
    out: &mut Vec<u8>,
    name: &[u8],
    method: u16,
    crc: u32,
    data: &[u8],
    size: usize,
    offset: u32,
) {
    out.extend_from_slice(&0x02014b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&method.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&u32::try_from(size).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&(u16::try_from(name.len()).unwrap_or(u16::MAX)).to_le_bytes());
    out.extend_from_slice(&[0; 6]);
    out.extend_from_slice(&[0; 6]);
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(name);
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _bit in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb88320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
