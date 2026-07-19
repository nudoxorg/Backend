//! Shared HTTP client for all upstream registry requests, plus catalog follower
//! infrastructure for the mirror demand-pull path.
//!
//! One `reqwest::Client` instance with the canonical User-Agent, per-language
//! token-bucket rate limiting (rates sourced from [`ecosystem::spec`]),
//! retry-with-backoff on 429/5xx, and optional `Retry-After` honor per policy.
//!
//! M6: replaces ad-hoc `reqwest::get` / `reqwest::Client::new()` calls in
//! `resolve.rs` and `indexing.rs`.
//!
//! Phase 7 (M2/M4): adds [`catalog`] (follower trait), [`nuget_catalog`]
//! (NuGet V3 reference impl), and [`crates_catalog`] (crates.io V1 impl).

pub mod catalog;
pub mod crates_catalog;
pub mod nuget_catalog;

pub use catalog::{CatalogBatch, CatalogCursor, CatalogEvent, CatalogFollower, PollFuture};
pub use nuget_catalog::NuGetCatalogFollower;
pub use crates_catalog::CratesCatalogFollower;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::Mutex;

use ecosystem::{Language, LanguageExt as _};

const USER_AGENT: &str = "nudox-registry-mirror/0.1 (+https://github.com/philocalyst)";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const BASE_BACKOFF_MS: u64 = 250;
const MAX_ATTEMPTS: u32 = 4; // 1 initial + 3 retries

/// Shared upstream HTTP client with per-language rate limiting.
#[derive(Clone)]
pub struct UpstreamClient {
    inner: reqwest::Client,
    buckets: Arc<HashMap<Language, Mutex<TokenBucket>>>,
}

impl UpstreamClient {
    /// Construct a client with a per-language token bucket seeded from
    /// [`ecosystem::spec`] policy.
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
            buckets.insert(lang, Mutex::new(TokenBucket::new(f64::from(policy.max_requests_per_second))));
        }
        Self { inner, buckets: Arc::new(buckets) }
    }

    /// GET `url` on behalf of `language`, applying rate limiting and retry.
    ///
    /// Retries on 429/5xx and transport errors up to `MAX_ATTEMPTS` attempts.
    /// Honors `Retry-After` when the ecosystem policy requests it.
    pub async fn get(&self, language: Language, url: &str) -> Result<Bytes, UpstreamError> {
        let policy = language.spec().policy();

        for attempt in 0..MAX_ATTEMPTS {
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
                    if attempt + 1 >= MAX_ATTEMPTS {
                        return Err(UpstreamError::Transport(e));
                    }
                    tokio::time::sleep(backoff(attempt, url)).await;
                    continue;
                }
                Ok(resp) => {
                    let status = resp.status();

                    // Honor Retry-After on 429 when policy says so.
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        if attempt + 1 >= MAX_ATTEMPTS {
                            return Err(UpstreamError::RateLimited);
                        }
                        let wait = if policy.respect_retry_after {
                            retry_after_secs(&resp).map(Duration::from_secs)
                        } else {
                            None
                        };
                        tokio::time::sleep(wait.unwrap_or_else(|| backoff(attempt, url))).await;
                        continue;
                    }

                    if status.is_server_error() {
                        if attempt + 1 >= MAX_ATTEMPTS {
                            return Err(UpstreamError::ServerError(status.as_u16()));
                        }
                        tokio::time::sleep(backoff(attempt, url)).await;
                        continue;
                    }

                    if status == reqwest::StatusCode::NOT_FOUND {
                        return Err(UpstreamError::NotFound);
                    }

                    let bytes = resp
                        .error_for_status()
                        .map_err(UpstreamError::Transport)?
                        .bytes()
                        .await
                        .map_err(UpstreamError::Transport)?;
                    return Ok(bytes);
                }
            }
        }
        Err(UpstreamError::RetriesExhausted)
    }
}

impl Default for UpstreamClient {
    fn default() -> Self { Self::new() }
}

// ── UpstreamError ────────────────────────────────────────────────────────────

/// Error from a single upstream GET.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
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

impl heart::Retryable for UpstreamError {
    fn is_retryable(&self) -> bool {
        matches!(
            self,
            UpstreamError::Transport(_) | UpstreamError::ServerError(_) | UpstreamError::RateLimited
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
        Self { interval_ns, next_available: Instant::now() }
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
    let hash: u64 = url.bytes().fold(attempt as u64 ^ 0xDEADBEEF, |acc, b| {
        acc.wrapping_mul(6364136223846793005).wrapping_add(b as u64 ^ 1442695040888963407)
    });
    // jitter: ±25% of base_ms.
    let jitter_range = base_ms / 4;
    let jitter = if jitter_range == 0 { 0 } else { hash % (jitter_range * 2) };
    let ms = base_ms.saturating_sub(jitter_range).saturating_add(jitter);
    Duration::from_millis(ms)
}

/// Parse `Retry-After` header as integer seconds, if present.
fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    let val = resp.headers().get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    val.trim().parse::<u64>().ok()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::{Router, response::IntoResponse, routing::get};

    async fn run_test_server(
        router: Router,
    ) -> (tokio::task::JoinHandle<()>, std::net::SocketAddr) {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });
        (handle, addr)
    }

    #[tokio::test]
    async fn ua_header_is_sent() {
        #[derive(Clone)]
        struct Captured(Arc<Mutex<Option<String>>>);

        let captured = Captured(Arc::new(Mutex::new(None)));
        let captured_clone = captured.clone();

        let router = Router::new()
            .route(
                "/test",
                get(move |headers: axum::http::HeaderMap| {
                    let cap = captured_clone.clone();
                    async move {
                        let ua = headers
                            .get(axum::http::header::USER_AGENT)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned);
                        *cap.0.lock().await = ua;
                        "ok"
                    }
                }),
            );

        let (_handle, addr) = run_test_server(router).await;
        let client = UpstreamClient::new();
        let url = format!("http://{addr}/test");
        let _ = client.get(Language::Rust, &url).await;
        let ua = captured.0.lock().await.clone();
        assert_eq!(ua.as_deref(), Some(USER_AGENT));
    }

    #[tokio::test]
    async fn retry_after_honored_on_429() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = call_count.clone();

        let router = Router::new().route(
            "/rate",
            get(move || {
                let count = cc.clone();
                async move {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        (
                            axum::http::StatusCode::TOO_MANY_REQUESTS,
                            [(axum::http::header::RETRY_AFTER, "0")],
                            "retry",
                        )
                            .into_response()
                    } else {
                        (axum::http::StatusCode::OK, "ok").into_response()
                    }
                }
            }),
        );

        let (_handle, addr) = run_test_server(router).await;
        let client = UpstreamClient::new();
        let url = format!("http://{addr}/rate");
        let result = client.get(Language::Rust, &url).await;
        assert!(result.is_ok(), "expected success after retry, got: {result:?}");
        assert!(call_count.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn retries_exhausted_returns_error() {
        let router = Router::new().route(
            "/fail",
            get(|| async {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "error").into_response()
            }),
        );

        let (_handle, addr) = run_test_server(router).await;
        let client = UpstreamClient::new();
        let url = format!("http://{addr}/fail");
        let result = client.get(Language::Rust, &url).await;
        assert!(
            matches!(result, Err(UpstreamError::RetriesExhausted) | Err(UpstreamError::ServerError(_))),
            "expected exhausted/server-error, got: {result:?}"
        );
    }
}
