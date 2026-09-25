//! Shared HTTP client for all upstream registry requests, plus catalog follower
//! infrastructure for the mirror demand-pull path.
//!
//! One `reqwest::Client` instance with the canonical User-Agent, per-language
//! token-bucket rate limiting (rates sourced from [`crate::ecosystem::spec`]),
//! retry-with-backoff on 429/5xx, and optional `Retry-After` honor per policy.
//!
//! M6: replaces ad-hoc `reqwest::get` / `reqwest::Client::new()` calls in
//! `resolve.rs` and `indexing.rs`.
//!
//! Phase 7 (M2/M4): adds [`catalog`] (follower trait), [`nuget_catalog`]
//! (NuGet V3 reference impl), and [`crates_catalog`] (crates.io V1 impl).
pub mod catalog;
pub mod crates_catalog;
pub mod go_index;
pub mod maven_search;
pub mod npm_changes;
pub mod nuget_catalog;
pub mod percent;
pub mod pypi_updates;

pub use catalog::{
    CatalogBatch, CatalogCursor, CatalogEvent, CatalogFollower, DocumentFacts, PollFuture,
    attach_document_dependencies, merge_document_facts,
};
pub use crates_catalog::CratesCatalogFollower;
pub use go_index::GoIndexFollower;
pub use maven_search::MavenSearchFollower;
pub use npm_changes::NpmChangesFollower;
pub use nuget_catalog::NuGetCatalogFollower;
pub use percent::path_segment;
pub use pypi_updates::{PypiUpdatesFollower, document_facts as pypi_document_facts};

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use tokio::sync::Mutex;

use crate::ecosystem::{Language, LanguageExt as _};

const USER_AGENT: &str = "nudox-registry-mirror/0.1 (+https://github.com/philocalyst)";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const BASE_BACKOFF_MS: u64 = 250;

/// Shared upstream HTTP client with per-language rate limiting.
#[derive(Clone)]
pub struct UpstreamClient {
    inner: reqwest::Client,
    buckets: Arc<HashMap<Language, Mutex<TokenBucket>>>,
}

impl UpstreamClient {
    /// Construct a client with a per-language token bucket seeded from
    /// [`crate::ecosystem::spec`] policy.
    pub fn new() -> Self {
        use strum::IntoEnumIterator;
        let inner = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(CONNECT_TIMEOUT)
            .build()
            .expect("upstream client construction cannot fail with valid params");

        let mut buckets = HashMap::new();
        for lang in Language::iter() {
            let policy = lang.spec().policy();
            buckets.insert(
                lang,
                Mutex::new(TokenBucket::new(f64::from(policy.max_requests_per_second))),
            );
        }
        Self {
            inner,
            buckets: Arc::new(buckets),
        }
    }

    /// GET `url` on behalf of `language`, applying rate limiting and retry.
    ///
    /// The first request plus the ecosystem `retry_budget` retries run.
    /// 429 and 5xx, and transport errors, consume that budget. When the
    /// policy asks, `Retry-After` on 429 and 503 replaces the computed backoff.
    pub async fn get(&self, language: Language, url: &str) -> Result<Bytes, Error> {
        let policy = language.spec().policy();
        let limit = attempt_limit(policy.retry_budget);

        for attempt in 0..limit {
            // Rate limiting: wait if the token bucket is depleted.
            if let Some(bucket) = self.buckets.get(&language) {
                let wait = bucket.lock().await.try_consume();
                if let Some(wait_dur) = wait {
                    tokio::time::sleep(wait_dur).await;
                }
            }

            let response = self.inner.get(url).send().await;
            match response {
                Err(e) => {
                    if attempt + 1 >= limit {
                        return Err(Error::Transport(e));
                    }
                    tokio::time::sleep(backoff(attempt, url)).await;
                }
                Ok(resp) => {
                    let status = resp.status();
                    let code = status.as_u16();
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
                    {
                        let header = retry_after_secs(&resp);
                        match retry_wait(
                            policy.respect_retry_after,
                            code,
                            header,
                            attempt,
                            limit,
                            url,
                        ) {
                            RetryWait::GiveUp
                                if status == reqwest::StatusCode::TOO_MANY_REQUESTS =>
                            {
                                return Err(Error::RateLimited);
                            }
                            RetryWait::GiveUp => return Err(Error::ServerError(code)),
                            RetryWait::Again(wait) => {
                                tokio::time::sleep(wait).await;
                                continue;
                            }
                        }
                    }

                    if status == reqwest::StatusCode::NOT_FOUND {
                        return Err(Error::NotFound);
                    }

                    let bytes = resp
                        .error_for_status()
                        .map_err(Error::Transport)?
                        .bytes()
                        .await
                        .map_err(Error::Transport)?;
                    return Ok(bytes);
                }
            }
        }
        Err(Error::RetriesExhausted)
    }
}

impl Default for UpstreamClient {
    fn default() -> Self {
        Self::new()
    }
}

// ── Error ────────────────────────────────────────────────────────────

/// Error from a single upstream GET.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("upstream returned 404 not found")]
    NotFound,

    #[error("upstream returned {0} server error")]
    ServerError(u16),

    #[error("upstream returned 429 rate limited")]
    RateLimited,

    #[error("all retry attempts exhausted")]
    RetriesExhausted,

    /// A response body could not be parsed as expected.
    #[error("upstream response parse error: {0}")]
    Parse(String),
}

/// Backwards-compatible alias: the upstream error (now [`Error`]).
pub use self::Error as UpstreamError;

impl heart::Retryable for Error {
    fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::Transport(_) | Error::ServerError(_) | Error::RateLimited
        )
    }
}

// ── TokenBucket ─────────────────────────────────────────────────────────────

/// A simple Instant-based token bucket. One token is consumed per request;
/// if the bucket is empty, reports how long to wait before retrying.
struct TokenBucket {
    /// Minimum gap between requests in nanoseconds.
    interval_ns: u64,
    /// When the next token is available.
    next_available: Instant,
}

impl TokenBucket {
    fn new(max_rps: f64) -> Self {
        let interval_ns = if max_rps <= 0.0 {
            0
        } else {
            (1_000_000_000.0 / max_rps) as u64
        };
        Self {
            interval_ns,
            next_available: Instant::now(),
        }
    }

    /// Try to consume a token. Returns `None` if one is available immediately,
    /// or `Some(wait)` if the caller should sleep first.
    fn try_consume(&mut self) -> Option<Duration> {
        let now = Instant::now();
        if now >= self.next_available {
            self.next_available = now + Duration::from_nanos(self.interval_ns);
            None
        } else {
            let wait = self.next_available - now;
            self.next_available += Duration::from_nanos(self.interval_ns);
            Some(wait)
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Backoff duration for `attempt` (0-based), with ±25% jitter derived from
/// a hash of `(attempt, url)` — no rand dependency.
fn backoff(attempt: u32, url: &str) -> Duration {
    let base_ms = BASE_BACKOFF_MS.saturating_mul(1u64 << attempt.min(6));
    // Simple hash for deterministic jitter: fold url bytes + attempt.
    let hash: u64 = url
        .bytes()
        .fold(u64::from(attempt) ^ 0xdead_beef, |acc, b| {
            acc.wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(u64::from(b) ^ 0x1405_7b7e_f767_814f)
        });
    // jitter: ±25% of base_ms.
    let jitter_range = base_ms / 4;
    let jitter = if jitter_range == 0 {
        0
    } else {
        hash % (jitter_range * 2)
    };
    let ms = base_ms.saturating_sub(jitter_range).saturating_add(jitter);
    Duration::from_millis(ms)
}

/// First request plus `retry_budget` retries.
fn attempt_limit(retry_budget: u8) -> u32 {
    u32::from(retry_budget).saturating_add(1)
}

/// Whether a transient status spends another attempt, and how long to wait.
enum RetryWait {
    GiveUp,
    Again(Duration),
}

/// `retry_after` is the `Retry-After` header in seconds, when it parsed.
///
/// 429 and 503 consult that header when `respect_retry_after` is set. Other
/// 5xx statuses use [`backoff`]. A spent budget gives up.
fn retry_wait(
    respect_retry_after: bool,
    status: u16,
    retry_after: Option<u64>,
    attempt: u32,
    limit: u32,
    url: &str,
) -> RetryWait {
    let transient = status == 429 || (500..600).contains(&status);
    if !transient || attempt + 1 >= limit {
        return RetryWait::GiveUp;
    }
    let header = (respect_retry_after && (status == 429 || status == 503))
        .then_some(retry_after)
        .flatten()
        .map(Duration::from_secs);
    RetryWait::Again(header.unwrap_or_else(|| backoff(attempt, url)))
}

/// Parse `Retry-After` header as integer seconds, if present.
fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    let val = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    val.trim().parse::<u64>().ok()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{RetryWait, attempt_limit, backoff, retry_wait};

    #[test]
    fn budget_is_the_attempt_limit() {
        assert_eq!(attempt_limit(0), 1);
        assert_eq!(attempt_limit(3), 4);
        assert_eq!(attempt_limit(u8::MAX), u32::from(u8::MAX) + 1);
    }

    #[test]
    fn spent_budget_gives_up_and_503_honors_retry_after() {
        assert!(matches!(
            retry_wait(true, 503, Some(12), 0, 1, "https://example"),
            RetryWait::GiveUp
        ));
        match retry_wait(true, 503, Some(12), 0, 4, "https://example") {
            RetryWait::Again(wait) => assert_eq!(wait.as_secs(), 12),
            RetryWait::GiveUp => panic!("503 with budget left must retry"),
        }
        match retry_wait(false, 503, Some(12), 0, 4, "https://example") {
            RetryWait::Again(wait) => assert_eq!(wait, backoff(0, "https://example")),
            RetryWait::GiveUp => panic!("503 with the header ignored must use backoff"),
        }
        match retry_wait(true, 500, Some(12), 0, 4, "https://example") {
            RetryWait::Again(wait) => assert_eq!(wait, backoff(0, "https://example")),
            RetryWait::GiveUp => panic!("500 must ignore Retry-After"),
        }
        match retry_wait(true, 429, Some(0), 2, 4, "https://example") {
            RetryWait::Again(wait) => assert_eq!(wait, std::time::Duration::ZERO),
            RetryWait::GiveUp => panic!("a zero Retry-After is a real wait"),
        }
        assert!(matches!(
            retry_wait(true, 404, Some(9), 0, 4, "https://example"),
            RetryWait::GiveUp
        ));
    }
}
