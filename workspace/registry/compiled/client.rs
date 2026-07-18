//! The verifying client for `POST /v1/compiled/lookup` (SMOLVM-PLAN §5, SV-6).
//!
//! # Trust model
//!
//! The lookup mapping is trusted because:
//! (a) writes are fleet-only (SV-6) — the only write path is the iroh
//!     SyncService apply-hook, never an HTTP surface; and
//! (b) the referenced change-set is verified AT IMPORT by Pijul content-
//!     addressing when pulled over iroh — the lookup response is a claim,
//!     the changes are the proof.
//!
//! This client validates the wire shape (hex fields parse into their newtypes,
//! generation_stamp is a valid 64-hex hash) and rejects malformed responses
//! with typed errors. It does NOT fetch or verify artifact bytes — that
//! happens during iroh pull.
//!
//! # Batching
//!
//! `lookup` accepts any number of keys and splits them into POSTs of at most
//! [`MAX_KEYS_PER_REQUEST`] (the server-enforced §5.1 ceiling).

use heart::JobKey;

use super::{ChangeId, ChannelName, CompiledHit, LookupResult};

/// The server-side per-request key ceiling (SMOLVM-PLAN §5.1; mirrored by the
/// server's `COMPILED_LOOKUP_MAX_KEYS`). Larger batches are split.
pub const MAX_KEYS_PER_REQUEST: usize = 1024;

// ── Wire mirror ───────────────────────────────────────────────────────────────

/// The wire shape of one lookup response entry, as this client consumes it.
///
/// Fields are public (and `Serialize`) so tests can construct entries and
/// drive [`verify_entry`] directly.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WireEntry {
    /// The echoed job key, lower-hex.
    pub job_key: String,
    /// Hit/miss discriminator.
    pub hit: bool,
    /// The package id string. Present on hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Pijul channel name. Present on hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// 64-char lowercase hex tip change-hash. Present on hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
    /// Hash① of the generation (64-char lower-hex). Present on hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_stamp: Option<String>,
}

/// The wire shape of the whole response body.
#[derive(Debug, serde::Deserialize)]
struct WireResponse {
    results: Vec<WireEntry>,
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Why a hit entry failed verification.
#[derive(Debug, thiserror::Error)]
pub enum VerificationFailure {
    /// The response echoed a different job key than the one requested.
    #[error("echoed job_key {echoed:?} does not match the requested key")]
    KeyEcho { echoed: String },

    /// A hit entry was missing one of its mandatory fields.
    #[error("hit entry is missing required field {field:?}")]
    MissingField { field: &'static str },

    /// The `tip` field was not 64 lowercase hex chars.
    #[error("tip is not a valid 64-char lowercase hex change id")]
    BadTipHex,

    /// The `generation_stamp` field was not 64 lowercase hex chars.
    #[error("generation_stamp is not a valid 64-char lowercase hex digest")]
    BadStampHex,

    /// The `channel` field was empty.
    #[error("channel name must be non-empty")]
    EmptyChannel,
}

/// Errors from [`CompiledClient::lookup`].
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The HTTP round-trip itself failed (connect, timeout, body read, JSON).
    #[error("compiled lookup transport error")]
    Http(#[from] reqwest::Error),

    /// The server answered with a non-success status.
    #[error("compiled lookup returned status {status}")]
    UnexpectedStatus { status: reqwest::StatusCode },

    /// The server returned a different number of results than keys requested.
    #[error("compiled lookup returned {got} results for {expected} keys")]
    ResponseShape { expected: usize, got: usize },

    /// A hit failed shape validation.
    #[error("verification failed for job key {job_key}")]
    VerificationFailed {
        job_key: JobKey,
        #[source]
        reason: VerificationFailure,
    },
}

// ── Result type ───────────────────────────────────────────────────────────────

/// One verified per-key outcome.
#[derive(Debug)]
pub struct Looked {
    pub job_key: JobKey,
    pub result: LookupResult,
}

// ── Client ────────────────────────────────────────────────────────────────────

/// The HTTP client for the no-double-compile handshake.
pub struct CompiledClient {
    base_url: url::Url,
    http: reqwest::Client,
}

impl CompiledClient {
    pub fn new(base_url: url::Url) -> Self {
        Self { base_url, http: reqwest::Client::new() }
    }

    pub fn with_http(base_url: url::Url, http: reqwest::Client) -> Self {
        Self { base_url, http }
    }

    /// Look up `keys` against the fleet. Batches of more than
    /// [`MAX_KEYS_PER_REQUEST`] keys are split into multiple sequential POSTs;
    /// results come back in input order, one [`Looked`] per key.
    pub async fn lookup(&self, keys: &[JobKey]) -> Result<Vec<Looked>, ClientError> {
        let endpoint = self.endpoint();
        let mut looked = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(MAX_KEYS_PER_REQUEST.max(1)) {
            let hex_keys: Vec<String> = chunk.iter().map(JobKey::hex).collect();
            let response = self
                .http
                .post(endpoint.clone())
                .json(&serde_json::json!({ "job_keys": hex_keys }))
                .send()
                .await?;
            let status = response.status();
            if !status.is_success() {
                return Err(ClientError::UnexpectedStatus { status });
            }
            let body: WireResponse = response.json().await?;
            if body.results.len() != chunk.len() {
                return Err(ClientError::ResponseShape {
                    expected: chunk.len(),
                    got: body.results.len(),
                });
            }
            for (&job_key, entry) in chunk.iter().zip(&body.results) {
                let result = verify_entry(job_key, entry)
                    .map_err(|reason| ClientError::VerificationFailed { job_key, reason })?;
                looked.push(Looked { job_key, result });
            }
        }
        Ok(looked)
    }

    fn endpoint(&self) -> url::Url {
        let mut url = self.base_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .expect("compiled-lookup base URLs are http(s) and can-be-a-base");
            segments.pop_if_empty().extend(["v1", "compiled", "lookup"]);
        }
        url
    }
}

// ── Verification ──────────────────────────────────────────────────────────────

/// Verify one response entry against the key it must answer.
///
/// Validates hex fields parse into their newtypes. On success a hit becomes a
/// [`CompiledHit`] with typed fields. On a miss returns `LookupResult::Miss`.
pub fn verify_entry(
    job_key: JobKey,
    entry: &WireEntry,
) -> Result<LookupResult, VerificationFailure> {
    if entry.job_key != job_key.hex() {
        return Err(VerificationFailure::KeyEcho { echoed: entry.job_key.clone() });
    }
    if !entry.hit {
        return Ok(LookupResult::Miss);
    }

    fn require<'e>(
        field: Option<&'e String>,
        name: &'static str,
    ) -> Result<&'e String, VerificationFailure> {
        field.ok_or(VerificationFailure::MissingField { field: name })
    }

    let channel_str = require(entry.channel.as_ref(), "channel")?;
    let tip_str = require(entry.tip.as_ref(), "tip")?;
    let stamp_str = require(entry.generation_stamp.as_ref(), "generation_stamp")?;

    let channel =
        ChannelName::new(channel_str.clone()).map_err(|_| VerificationFailure::EmptyChannel)?;

    let tip = ChangeId::new(tip_str.clone()).map_err(|_| VerificationFailure::BadTipHex)?;

    let generation_stamp = decode_hash(stamp_str, "generation_stamp")
        .map_err(|_| VerificationFailure::BadStampHex)?;

    let package_str = require(entry.package.as_ref(), "package")?;
    let package_uuid = uuid::Uuid::parse_str(package_str)
        .map_err(|_| VerificationFailure::MissingField { field: "package" })?;
    let package = heart::PackageId::from_uuid(package_uuid);

    Ok(LookupResult::Hit(Box::new(CompiledHit {
        package,
        channel,
        tip,
        generation_stamp,
    })))
}

/// Decode a 64-char lower-hex digest field into a [`heart::content::ContentHash`].
fn decode_hash(
    hex: &str,
    _field: &'static str,
) -> Result<heart::content::ContentHash, ()> {
    if hex.len() != 64 {
        return Err(());
    }
    let mut raw = [0u8; 32];
    data_encoding::HEXLOWER.decode_mut(hex.as_bytes(), &mut raw).map_err(|_| ())?;
    Ok(heart::content::ContentHash::from_bytes(raw))
}
