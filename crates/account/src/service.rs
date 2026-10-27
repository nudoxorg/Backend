//! The three `api.nudox.org` calls a signed-in surface makes.
//!
//! ```text
//! GET  /v1/me            Authorization: Bearer ndx_…  → 200 account | 401 bad key
//! GET  /v1/usage         Authorization: Bearer ndx_…  → 200 usage   | 401 bad key
//! POST /v1/usage/record  Authorization: Bearer ndx_…  {"kind":"tool_call","count":n}
//!                        → 204 recorded | 401 bad key | 403 plan inactive | 429 limit
//! ```
//!
//! The service owns every pricing rule: which tiers are capped, metered or
//! unlimited, and when an enterprise plan has lapsed. `record` refuses at the
//! cap itself, so no limit is repeated here.

use crate::key::ApiKey;
use serde::Deserialize;
use std::time::Duration;

/// The production auth service.
pub const DEFAULT_BASE_URL: &str = "https://api.nudox.org";
/// Overrides [`DEFAULT_BASE_URL`] in debug builds, for tests and local auth.
pub const BASE_URL_ENV: &str = "NUDOX_API_URL";
/// The longest reply body any of these endpoints sends.
const MOST_REPLY_BYTES: u64 = 64 * 1024;

/// The signed-in account, from `GET /v1/me`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Account {
    /// The account's id.
    pub user_id: u64,
    /// The sign-in email.
    #[serde(default)]
    pub email: String,
    /// `free`, `indie`, `team` or `enterprise`.
    #[serde(default)]
    pub tier: String,
    /// `enterprise` for an enterprise account, even after it lapses.
    #[serde(default)]
    pub plan: Option<String>,
}

impl Account {
    /// The plan as a settings page names it.
    #[must_use]
    pub fn plan_name(&self) -> String {
        let plan = self.plan.as_deref().unwrap_or(&self.tier);
        let mut letters = plan.chars();
        letters.next().map_or_else(
            || "Free".to_owned(),
            |first| first.to_uppercase().chain(letters).collect(),
        )
    }
}

/// This billing period's usage, from `GET /v1/usage`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Usage {
    /// The tier the limits below belong to.
    #[serde(default)]
    pub tier: String,
    /// MCP tool calls this period.
    #[serde(default)]
    pub tool_calls: i64,
    /// Every recorded unit this period; this is what the limit counts.
    #[serde(default)]
    pub used: i64,
    /// The period's cap or included quota; absent when unlimited.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Units left before the limit.
    #[serde(default)]
    pub remaining: Option<i64>,
    /// Whether `used` has reached `limit`.
    #[serde(default)]
    pub over_limit: bool,
    /// Whether overage is billed rather than blocked once opted in.
    #[serde(default)]
    pub metered: bool,
    /// Whether the account opted into overage billing.
    #[serde(default)]
    pub overage_opt_in: Option<bool>,
    /// Whether the subscription is in its free trial.
    #[serde(default)]
    pub trialing: bool,
}

impl Usage {
    /// One line: "312 of 5,000 this period", or "312 this period" when unlimited.
    #[must_use]
    pub fn summary(&self) -> String {
        match self.limit {
            Some(limit) => format!("{} of {} this period", grouped(self.used), grouped(limit)),
            None => format!("{} this period", grouped(self.used)),
        }
    }
}

fn grouped(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if value < 0 {
        out.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Why a read failed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServiceError {
    /// The service does not know the key: it was revoked, rotated or mistyped.
    #[error("this API key is not valid (it may have been revoked or rotated)")]
    InvalidKey,
    /// The service could not be reached.
    #[error("cannot reach the Nudox account service: {0}")]
    Unreachable(String),
    /// The service answered something this client does not understand.
    #[error("the Nudox account service answered {status}: {detail}")]
    Unexpected {
        /// HTTP status.
        status: u16,
        /// Bounded reply text.
        detail: String,
    },
}

/// Why a tool call could not be recorded.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RecordError {
    /// The period's limit is reached; the service's sentence says what to do.
    #[error("{0}")]
    LimitReached(String),
    /// The plan is inactive (for example, a lapsed enterprise subscription).
    #[error("{0}")]
    Inactive(String),
    /// See [`ServiceError`].
    #[error(transparent)]
    Service(#[from] ServiceError),
}

/// A client for the auth service.
#[derive(Clone, Debug)]
pub struct Service {
    agent: ureq::Agent,
    base: String,
}

impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}

impl Service {
    /// The production service, or [`BASE_URL_ENV`] in a debug build.
    #[must_use]
    pub fn new() -> Self {
        let configured = cfg!(debug_assertions)
            .then(|| std::env::var(BASE_URL_ENV).ok())
            .flatten();
        Self::at(configured.as_deref().unwrap_or(DEFAULT_BASE_URL))
    }

    /// A service at `base`, such as a test server.
    #[must_use]
    pub fn at(base: &str) -> Self {
        // Short timeouts: an MCP tool call waits on `record`, and an
        // unreachable service must fall through to the offline grace quickly.
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(3)))
            .timeout_global(Some(Duration::from_secs(8)))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .new_agent();
        Self {
            agent,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    /// The account the key belongs to. Signing in calls this to check a key.
    ///
    /// # Errors
    /// See [`ServiceError`].
    pub fn account(&self, key: &ApiKey) -> Result<Account, ServiceError> {
        self.read_json("/v1/me", key)
    }

    /// This period's usage.
    ///
    /// # Errors
    /// See [`ServiceError`].
    pub fn usage(&self, key: &ApiKey) -> Result<Usage, ServiceError> {
        self.read_json("/v1/usage", key)
    }

    /// Records `count` MCP tool calls.
    ///
    /// # Errors
    /// See [`RecordError`].
    pub fn record_tool_calls(&self, key: &ApiKey, count: i64) -> Result<(), RecordError> {
        let response = self
            .agent
            .post(format!("{}/v1/usage/record", self.base))
            .header("Authorization", format!("Bearer {}", key.expose()))
            .send_json(serde_json::json!({ "kind": "tool_call", "count": count }))
            .map_err(unreachable)?;
        let status = response.status().as_u16();
        match status {
            200..=299 => Ok(()),
            401 => Err(ServiceError::InvalidKey.into()),
            403 => Err(RecordError::Inactive(body_text(response))),
            429 => Err(RecordError::LimitReached(body_text(response))),
            _ => Err(ServiceError::Unexpected {
                status,
                detail: body_text(response),
            }
            .into()),
        }
    }

    fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        key: &ApiKey,
    ) -> Result<T, ServiceError> {
        let mut response = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", key.expose()))
            .header("Accept", "application/json")
            .call()
            .map_err(unreachable)?;
        let status = response.status().as_u16();
        match status {
            200 => response
                .body_mut()
                .with_config()
                .limit(MOST_REPLY_BYTES)
                .read_json()
                .map_err(|error| ServiceError::Unexpected {
                    status,
                    detail: error.to_string(),
                }),
            401 => Err(ServiceError::InvalidKey),
            _ => Err(ServiceError::Unexpected {
                status,
                detail: body_text(response),
            }),
        }
    }
}

fn unreachable(error: ureq::Error) -> ServiceError {
    ServiceError::Unreachable(error.to_string())
}

fn body_text(mut response: ureq::http::Response<ureq::Body>) -> String {
    let text = response
        .body_mut()
        .with_config()
        .limit(MOST_REPLY_BYTES)
        .read_to_string()
        .unwrap_or_default();
    let text = text.trim();
    let mut bounded: String = text.chars().take(300).collect();
    if bounded.is_empty() {
        bounded = format!("HTTP {}", response.status().as_u16());
    }
    bounded
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A one-thread HTTP server answering each request from `replies` in order,
    /// recording each request's first line and body.
    pub(crate) struct FakeService {
        pub(crate) base: String,
        pub(crate) seen: Arc<Mutex<Vec<String>>>,
    }

    impl FakeService {
        pub(crate) fn answering(replies: Vec<(u16, &'static str)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let base = format!("http://{}", listener.local_addr().expect("addr"));
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&seen);
            std::thread::spawn(move || {
                for (status, body) in replies {
                    let Ok((stream, _)) = listener.accept() else {
                        return;
                    };
                    let mut reader = BufReader::new(stream);
                    let mut first = String::new();
                    let _ = reader.read_line(&mut first);
                    let mut length = 0;
                    let mut authorized = false;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        let lower = line.to_ascii_lowercase();
                        if let Some(value) = lower.strip_prefix("content-length:") {
                            length = value.trim().parse().unwrap_or(0);
                        }
                        authorized |= lower.starts_with("authorization: bearer ndx_");
                    }
                    let mut request_body = vec![0; length];
                    let _ = reader.read_exact(&mut request_body);
                    let sent = serde_json::from_slice::<serde_json::Value>(&request_body)
                        .map_or_else(
                            |_| String::from_utf8_lossy(&request_body).into_owned(),
                            |value| value.to_string(),
                        );
                    log.lock()
                        .expect("log")
                        .push(format!("{} auth={authorized} {sent}", first.trim()));
                    let mut stream = reader.into_inner();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    // Close only our half and wait for the client's: dropping
                    // a socket with unread bytes resets it and loses the reply.
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Write);
                    let _ = std::io::copy(&mut stream, &mut std::io::sink());
                }
            });
            Self { base, seen }
        }

        pub(crate) fn requests(&self) -> Vec<String> {
            self.seen.lock().expect("log").clone()
        }
    }

    fn key() -> ApiKey {
        ApiKey::parse(crate::store::tests::KEY).expect("key")
    }

    #[test]
    fn reads_the_account_and_usage() {
        let fake = FakeService::answering(vec![
            (
                200,
                r#"{"user_id":24,"email":"a@b.c","tier":"indie","scopes":[],"orgs":[]}"#,
            ),
            (
                200,
                r#"{"tier":"indie","tool_calls":312,"used":340,"limit":5000,"remaining":4660,"over_limit":false}"#,
            ),
        ]);
        let service = Service::at(&fake.base);
        let account = service.account(&key()).expect("account");
        assert_eq!(account.email, "a@b.c");
        assert_eq!(account.plan_name(), "Indie");
        let usage = service.usage(&key()).expect("usage");
        assert_eq!(usage.tool_calls, 312);
        assert_eq!(usage.summary(), "340 of 5,000 this period");
        let requests = fake.requests();
        assert!(
            requests[0].starts_with("GET /v1/me HTTP/1.1 auth=true"),
            "{requests:?}"
        );
        assert!(
            requests[1].starts_with("GET /v1/usage HTTP/1.1 auth=true"),
            "{requests:?}"
        );
    }

    #[test]
    fn an_unknown_key_is_invalid_not_unreachable() {
        let fake = FakeService::answering(vec![(401, "invalid api key")]);
        assert_eq!(
            Service::at(&fake.base).account(&key()),
            Err(ServiceError::InvalidKey)
        );
    }

    #[test]
    fn records_tool_calls_and_passes_through_the_service_refusals() {
        let fake = FakeService::answering(vec![
            (204, ""),
            (
                429,
                "monthly usage limit exceeded (500/500 used, this request would add 1)",
            ),
            (403, "enterprise subscription is not active"),
        ]);
        let service = Service::at(&fake.base);
        service.record_tool_calls(&key(), 2).expect("recorded");
        assert!(matches!(
            service.record_tool_calls(&key(), 1),
            Err(RecordError::LimitReached(sentence)) if sentence.contains("500/500")
        ));
        assert!(matches!(
            service.record_tool_calls(&key(), 1),
            Err(RecordError::Inactive(_))
        ));
        let requests = fake.requests();
        assert!(
            requests[0].ends_with(r#"{"count":2,"kind":"tool_call"}"#),
            "{requests:?}"
        );
    }

    #[test]
    fn a_closed_port_is_unreachable() {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("port")
            .port();
        let service = Service::at(&format!("http://127.0.0.1:{port}"));
        assert!(matches!(
            service.account(&key()),
            Err(ServiceError::Unreachable(_))
        ));
    }

    #[test]
    fn plan_names_read_like_words() {
        let account = |tier: &str, plan: Option<&str>| Account {
            user_id: 1,
            email: String::new(),
            tier: tier.to_owned(),
            plan: plan.map(str::to_owned),
        };
        assert_eq!(account("free", None).plan_name(), "Free");
        assert_eq!(
            account("free", Some("enterprise")).plan_name(),
            "Enterprise"
        );
        assert_eq!(account("", None).plan_name(), "Free");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(999), "999");
    }
}
