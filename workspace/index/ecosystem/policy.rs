//! Upstream HTTP hygiene policy — every registry states its limits explicitly.

/// Rate-limit / retry budget for one ecosystem's upstream registry. Consumed
/// by `registry::upstream::UpstreamClient`; pure data here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UpstreamPolicy {
    /// Token-bucket refill rate (crates.io: 1.0; npm/pypi/maven: 10.0;
    /// nuget/goproxy: 20.0).
    pub max_requests_per_second: f32,

    /// Transient (5xx/429/transport) retries, exponential backoff.
    pub retry_budget: u8,

    /// Honor `Retry-After` on 429/503 (true everywhere today; stated per impl
    /// so the value is documented next to its peers).
    pub respect_retry_after: bool,
}
