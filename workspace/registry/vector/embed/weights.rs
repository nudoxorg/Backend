//! Pinned weights artifact policy (09c I11/I16, 09-vector §13.5).
//!
//! The canonical durable artifact is the sha-pinned fp32 ONNX file
//! (`model.onnx`, 641,517,466 bytes) placed on disk by the artifact fetcher —
//! this module never touches the network. Callers that hit
//! [`Error::MissingWeights`] must disable semantic search gracefully
//! (offline fail-soft), not block corpus readiness on a download.
//!
//! It was the int8 file (`model_quantized.onnx`, 161,895,621 bytes) until
//! 2026-08-08. That artifact is *dynamically* quantized, so its vectors depend
//! on batch composition and could never have been durable-canonical — see
//! [`crate::vector::core::model::CANONICAL_WEIGHTS_FILE`] for the measurements.
//! Verifying a hash proves you loaded the file you meant to; it says nothing
//! about whether that file can support the claims made about its output.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::vector::core::EmbeddingModel as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Where the model artifacts live and what the ONNX file must hash to.
///
/// `expected_sha256 = None` means "trust the file" (dev / bake-off builds);
/// production configs pin it (`semantic.weights_artifact = <file>@<sha256>`,
/// 09c §8.3) — an unpinned default in shipping config is a bug (I16).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightsSpec {
    /// Directory holding the ONNX file plus the tokenizer sidecar files
    /// (`tokenizer.json`, `config.json`, `special_tokens_map.json`,
    /// `tokenizer_config.json`).
    pub dir: PathBuf,

    /// The ONNX file name inside `dir`. Canonical: the brand's weights hint
    /// (`model.onnx` for JinaCodeV2).
    pub onnx_file: String,

    /// Pinned sha256 of the ONNX file, if the config pins one.
    pub expected_sha256: Option<[u8; 32]>,
}

impl WeightsSpec {
    /// A spec for the canonical JinaCodeV2 artifact under `dir`.
    ///
    /// Carries the brand's pinned sha256 through, rather than defaulting to
    /// `None`. I16 ("an unpinned default in shipping config is a bug") was
    /// stated here but not enforced anywhere: the brand held a pin and this
    /// constructor discarded it, so every caller silently got "trust the file".
    pub fn jina_code_v2(dir: impl Into<PathBuf>) -> Self {
        let hint = crate::vector::core::JinaCodeV2::weights_hint()
            .expect("JinaCodeV2 is self-hostable with a pinned ONNX artifact");
        Self {
            dir: dir.into(),
            onnx_file: hint.file.to_owned(),
            expected_sha256: hint.sha256,
        }
    }

    /// Pin the expected sha256 of the ONNX file.
    pub fn with_sha256(mut self, sha256: [u8; 32]) -> Self {
        self.expected_sha256 = Some(sha256);
        self
    }

    /// Absolute path of the ONNX file.
    pub fn onnx_path(&self) -> PathBuf {
        self.dir.join(&self.onnx_file)
    }

    /// Path of a tokenizer sidecar file inside `dir`.
    pub fn sidecar(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Verify the artifact: exists, readable, and — if pinned — sha256 matches.
    ///
    /// Streams the file through sha2 (the artifact is ~162 MB; never buffer it
    /// just to hash). On success returns the *actual* digest so callers can
    /// report it in `EmbedRuntimeInfo.weights_sha256` and fold it into
    /// `tool_digest` honestly, even when the config didn't pin one.
    pub fn verify(&self) -> Result<VerifiedWeights, Error> {
        let path = self.onnx_path();
        let file = File::open(&path).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => Error::MissingWeights { path: path.clone() },
            _ => Error::Io {
                path: path.clone(),
                source,
            },
        })?;

        let (sha256, bytes) = stream_sha256(file, &path)?;

        if let Some(expected) = self.expected_sha256
            && expected != sha256
        {
            return Err(Error::Sha256Mismatch {
                path,
                expected,
                actual: sha256,
            });
        }

        Ok(VerifiedWeights {
            path,
            sha256,
            bytes,
        })
    }
}

/// Proof that the on-disk artifact matched its pin (or was hashed unpinned).
#[derive(Debug, Clone)]
pub struct VerifiedWeights {
    /// Absolute path of the verified ONNX file.
    pub path: PathBuf,
    /// The actual sha256 of the file contents.
    pub sha256: [u8; 32],
    /// File size in bytes.
    pub bytes: u64,
}

/// Why the weights artifact is unusable.
///
/// [`Error::MissingWeights`] is the graceful-degradation signal:
/// semantic search off, everything else keeps working (09-vector §13.5).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The pinned artifact is not on disk. Disable semantic search; do not
    /// mark the corpus Ready (09b §16.9).
    #[error("model weights missing at {path} — semantic search disabled")]
    MissingWeights { path: PathBuf },

    /// The artifact exists but does not hash to its pin — corrupt or swapped.
    /// Never load it (I11): a wrong-weights vector poisons parity and CAS.
    #[error(
        "weights sha256 mismatch at {path}: expected {}, got {}",
        hex(expected),
        hex(actual)
    )]
    Sha256Mismatch {
        path: PathBuf,
        expected: [u8; 32],
        actual: [u8; 32],
    },

    /// The artifact could not be read.
    #[error("reading weights at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn stream_sha256(mut file: File, path: &Path) -> Result<([u8; 32], u64), Error> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf).map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hasher.finalize().into(), total))
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
