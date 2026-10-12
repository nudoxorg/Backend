//! Explicit fixed-revision acquisition. Query execution has no dependency on this module.

use super::local_preset::{MODEL_BYTES, MODEL_HASH, TOKENIZER_BYTES, TOKENIZER_HASH};
use super::{CandleArtifact, CandleSetupError};
use backend_platform::DirectoryCapability;
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const MODEL_URL: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/1110a243fdf4706b3f48f1d95db1a4f5529b4d41/model.safetensors?download=true";
const TOKENIZER_URL: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/1110a243fdf4706b3f48f1d95db1a4f5529b4d41/tokenizer.json?download=true";

/// Verified private staging artifacts. The directory and per-workspace acquisition lease live
/// through preparation/installation; dropping this value removes only its own private staging.
pub struct DownloadedCandleArtifacts {
    directory: tempfile::TempDir,
    _lease: File,
}
impl DownloadedCandleArtifacts {
    #[must_use]
    pub fn model(&self) -> PathBuf {
        self.directory.path().join("model.safetensors")
    }
    #[must_use]
    pub fn tokenizer(&self) -> PathBuf {
        self.directory.path().join("tokenizer.json")
    }
}

/// Downloads exactly the source-owned MiniLM revision into bounded private staging.
/// No executable is invoked and no runtime/config pointer is changed. Every acquired file is
/// length- and BLAKE3-verified before this guard is returned; failures remove partial artifacts.
pub fn download_candle_artifacts(
    compiler: &Path,
    mut progress: impl FnMut(CandleArtifact, u64, u64),
) -> Result<DownloadedCandleArtifacts, CandleSetupError> {
    backend_platform::durable::ensure_private_directory(compiler).map_err(CandleSetupError::Io)?;
    let parent = DirectoryCapability::open(compiler).map_err(CandleSetupError::Io)?;
    let lease = parent
        .open_private_file_read_write("embedding-download.lock", true)
        .map_err(CandleSetupError::Io)?;
    lease.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => CandleSetupError::SetupBusy,
        std::fs::TryLockError::Error(error) => CandleSetupError::Io(error),
    })?;
    let directory = tempfile::Builder::new()
        .prefix(".embedding-download-")
        .tempdir_in(compiler)
        .map_err(CandleSetupError::Io)?;
    let agent = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(300)))
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_body(Some(Duration::from_secs(30)))
        .max_redirects(5)
        .proxy(ureq::Proxy::try_from_env())
        .build()
        .new_agent();
    for (artifact, name, url, length, identity) in [
        (
            CandleArtifact::Model,
            "model.safetensors",
            MODEL_URL,
            MODEL_BYTES,
            MODEL_HASH,
        ),
        (
            CandleArtifact::Tokenizer,
            "tokenizer.json",
            TOKENIZER_URL,
            TOKENIZER_BYTES,
            TOKENIZER_HASH,
        ),
    ] {
        progress(artifact, 0, length as u64);
        let mut response = agent
            .get(url)
            .header("accept-encoding", "identity")
            .call()
            .map_err(|_| {
                CandleSetupError::Io(io::Error::other(
                    "pinned embedding artifact download failed",
                ))
            })?;
        if response.status().as_u16() != 200 {
            return Err(CandleSetupError::Io(io::Error::other(
                "pinned embedding artifact download returned a non-success status",
            )));
        }
        let path = directory.path().join(name);
        let mut destination = File::create_new(path).map_err(CandleSetupError::Io)?;
        let mut reported = 0;
        stream_verified(
            response.body_mut().as_reader(),
            &mut destination,
            artifact,
            length,
            identity,
            |bytes| {
                if bytes == length as u64 || bytes.saturating_sub(reported) >= 4 * 1024 * 1024 {
                    progress(artifact, bytes, length as u64);
                    reported = bytes;
                }
            },
        )?;
        destination.sync_all().map_err(CandleSetupError::Io)?;
    }
    Ok(DownloadedCandleArtifacts {
        directory,
        _lease: lease,
    })
}

fn stream_verified(
    mut input: impl Read,
    output: &mut impl Write,
    artifact: CandleArtifact,
    expected: usize,
    identity: [u8; 32],
    mut progress: impl FnMut(u64),
) -> Result<(), CandleSetupError> {
    let mut hash = blake3::Hasher::new();
    let mut count = 0usize;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        // Read at most one byte beyond the exact expected extent, never an unbounded body.
        let bound = chunk
            .len()
            .min(expected.saturating_sub(count).saturating_add(1));
        let read = input
            .read(&mut chunk[..bound])
            .map_err(CandleSetupError::Io)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read)
            .ok_or(CandleSetupError::ResourcePolicy)?;
        if count > expected {
            return Err(CandleSetupError::ArtifactLength {
                artifact,
                observed: count as u64,
                expected,
            });
        }
        output
            .write_all(&chunk[..read])
            .map_err(CandleSetupError::Io)?;
        hash.update(&chunk[..read]);
        progress(count as u64);
    }
    if count != expected {
        return Err(CandleSetupError::ArtifactLength {
            artifact,
            observed: count as u64,
            expected,
        });
    }
    if hash.finalize().as_bytes() != &identity {
        return Err(CandleSetupError::ArtifactIdentity(artifact));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_acquisition_requires_exact_extent_hash_and_complete_write() {
        let digest = *blake3::hash(b"abc").as_bytes();
        for input in [b"ab".as_slice(), b"abcd".as_slice()] {
            let mut output = Vec::new();
            assert!(matches!(
                stream_verified(input, &mut output, CandleArtifact::Model, 3, digest, |_| {}),
                Err(CandleSetupError::ArtifactLength { .. })
            ));
            assert!(
                output.len() <= 3,
                "overlong stream cannot grow staging beyond its bound"
            );
        }
        assert!(matches!(
            stream_verified(
                b"abd".as_slice(),
                &mut Vec::new(),
                CandleArtifact::Model,
                3,
                digest,
                |_| {}
            ),
            Err(CandleSetupError::ArtifactIdentity(CandleArtifact::Model))
        ));
        let mut destination = Vec::new();
        stream_verified(
            b"abc".as_slice(),
            &mut destination,
            CandleArtifact::Model,
            3,
            digest,
            |_| {},
        )
        .expect("exact bytes");
        assert_eq!(destination, b"abc");
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected disk refusal"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(matches!(
            stream_verified(
                b"abc".as_slice(),
                &mut BrokenWriter,
                CandleArtifact::Model,
                3,
                digest,
                |_| {}
            ),
            Err(CandleSetupError::Io(_))
        ));
    }
}
