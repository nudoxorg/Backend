//! What an MCP tool call asks before it runs.
//!
//! Every tool call is recorded with the service *before* the tool runs, and
//! the service's refusal (limit reached, plan inactive, key revoked) stops it.
//! That keeps every pricing rule in the service and makes a refused call cost
//! nothing.
//!
//! When the service cannot be reached, a key the service confirmed within
//! [`OFFLINE_GRACE`] keeps working and its calls are counted locally; the next
//! reachable call records them in one batch. If that batch would cross the
//! limit the service refuses it, and the backlog is dropped rather than
//! retried: offline calls are recorded at most once, never twice.

use crate::KEYS_URL;
use crate::key::ApiKey;
use crate::service::{RecordError, Service, ServiceError};
use crate::store::{KeyStore, StoreError};
use std::time::{Duration, Instant, SystemTime};

/// How long a confirmed key keeps working while the service is unreachable.
pub const OFFLINE_GRACE: Duration = Duration::from_hours(7 * 24);
/// After one unreachable attempt, later calls skip the network this long, so
/// an offline session does not pay a connect timeout per tool call.
const RETRY_AFTER: Duration = Duration::from_mins(1);
/// `verified-at` is rewritten at most this often while online.
const VERIFIED_REFRESH: Duration = Duration::from_hours(1);

/// A tool call's answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Admission {
    /// Run the tool.
    Allowed,
    /// Do not run it; tell the agent this sentence.
    Denied(String),
}

/// Records MCP tool calls against the signed-in account.
#[derive(Debug)]
pub struct ToolCallMeter {
    store: KeyStore,
    service: Service,
    offline_since: Option<Instant>,
}

impl ToolCallMeter {
    /// A meter over the shared account directory and the production service.
    ///
    /// # Errors
    /// Returns the store error when the account directory cannot be opened.
    pub fn open() -> Result<Self, StoreError> {
        Ok(Self::new(KeyStore::open()?, Service::new()))
    }

    /// A meter over an explicit store and service, for tests.
    #[must_use]
    pub fn new(store: KeyStore, service: Service) -> Self {
        Self {
            store,
            service,
            offline_since: None,
        }
    }

    /// Records one tool call, or says why it must not run.
    pub fn admit(&mut self) -> Admission {
        let key = match self.store.load() {
            Ok(Some((key, _))) => key,
            Ok(None) => return Admission::Denied(sign_in_sentence()),
            Err(error) => return Admission::Denied(format!("{error}. {}", sign_in_sentence())),
        };
        self.admit_with(&key, SystemTime::now(), Instant::now())
    }

    fn admit_with(&mut self, key: &ApiKey, now: SystemTime, clock: Instant) -> Admission {
        let recently_offline = self
            .offline_since
            .is_some_and(|since| clock.duration_since(since) < RETRY_AFTER);
        if recently_offline {
            return self.offline(now, "the account service was unreachable a moment ago");
        }
        let pending = self.store.pending();
        match self.service.record_tool_calls(key, pending + 1) {
            Ok(()) => self.recorded(now),
            Err(RecordError::LimitReached(_)) if pending > 0 => {
                // The backlog alone may cross the limit; drop it and ask for
                // this call by itself.
                self.store.clear_pending();
                match self.service.record_tool_calls(key, 1) {
                    Ok(()) => self.recorded(now),
                    Err(error) => self.refused(&error, now, clock),
                }
            }
            Err(error) => self.refused(&error, now, clock),
        }
    }

    fn recorded(&mut self, now: SystemTime) -> Admission {
        self.offline_since = None;
        self.store.clear_pending();
        let stale = self
            .store
            .verified_at()
            .is_none_or(|at| now.duration_since(at).unwrap_or(Duration::MAX) >= VERIFIED_REFRESH);
        if stale {
            self.store.mark_verified(now);
        }
        Admission::Allowed
    }

    fn refused(&mut self, error: &RecordError, now: SystemTime, clock: Instant) -> Admission {
        match error {
            RecordError::LimitReached(sentence) => Admission::Denied(format!(
                "Nudox usage limit reached: {sentence}. See your usage and plan at {KEYS_URL}."
            )),
            RecordError::Inactive(sentence) => Admission::Denied(format!(
                "Your Nudox plan is not active: {sentence}. Check billing at {KEYS_URL}."
            )),
            RecordError::Service(ServiceError::InvalidKey) => Admission::Denied(format!(
                "Your Nudox API key is no longer valid (it may have been revoked or rotated). {}",
                sign_in_sentence()
            )),
            // An unreachable or confused service is treated alike: run on
            // the offline grace rather than block the agent.
            RecordError::Service(error) => {
                self.offline_since = Some(clock);
                self.offline(now, &error.to_string())
            }
        }
    }

    fn offline(&self, now: SystemTime, detail: &str) -> Admission {
        let confirmed = self
            .store
            .verified_at()
            .and_then(|at| now.duration_since(at).ok());
        match confirmed {
            Some(age) if age < OFFLINE_GRACE => {
                self.store.set_pending(self.store.pending() + 1);
                Admission::Allowed
            }
            _ => Admission::Denied(format!(
                "Nudox cannot confirm your account ({detail}). Tool calls keep working offline for \
                 7 days after the last confirmation; connect to the internet and try again."
            )),
        }
    }
}

fn sign_in_sentence() -> String {
    format!(
        "Sign in to use Nudox tools: open the Nudox app, or run `nudox auth login` with a key \
         from {KEYS_URL} (or set NUDOX_API_KEY for this MCP server)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::FakeService;
    use crate::store::tests::{KEY, private_store};

    fn key() -> ApiKey {
        ApiKey::parse(KEY).expect("key")
    }

    fn meter(fake: &FakeService, store: KeyStore) -> ToolCallMeter {
        ToolCallMeter::new(store, Service::at(&fake.base))
    }

    #[test]
    fn records_before_running_and_stops_at_the_limit() {
        let fake = FakeService::answering(vec![(204, ""), (429, "limit 500/500")]);
        let (_root, store) = private_store();
        let mut meter = meter(&fake, store);
        let now = SystemTime::now();
        assert_eq!(
            meter.admit_with(&key(), now, Instant::now()),
            Admission::Allowed
        );
        let Admission::Denied(sentence) = meter.admit_with(&key(), now, Instant::now()) else {
            panic!("limit must deny");
        };
        assert!(sentence.contains("limit 500/500"), "{sentence}");
        assert_eq!(fake.requests().len(), 2);
    }

    #[test]
    fn a_revoked_key_says_to_sign_in_again() {
        let fake = FakeService::answering(vec![(401, "invalid api key")]);
        let (_root, store) = private_store();
        let Admission::Denied(sentence) =
            meter(&fake, store).admit_with(&key(), SystemTime::now(), Instant::now())
        else {
            panic!("revoked must deny");
        };
        assert!(sentence.contains("nudox auth login"), "{sentence}");
    }

    #[test]
    fn offline_calls_count_locally_within_the_grace_and_flush_once() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("port")
            .port();
        let (_root, store) = private_store();
        let now = SystemTime::now();
        store.mark_verified(now - Duration::from_secs(60));
        let mut offline = ToolCallMeter::new(
            store.clone(),
            Service::at(&format!("http://127.0.0.1:{port}")),
        );
        let start = Instant::now();
        assert_eq!(offline.admit_with(&key(), now, start), Admission::Allowed);
        // Within RETRY_AFTER the network is skipped entirely.
        assert_eq!(offline.admit_with(&key(), now, start), Admission::Allowed);
        assert_eq!(store.pending(), 2);

        let fake = FakeService::answering(vec![(204, "")]);
        let mut online = meter(&fake, store.clone());
        assert_eq!(
            online.admit_with(&key(), now, Instant::now()),
            Admission::Allowed
        );
        assert_eq!(store.pending(), 0);
        assert!(
            fake.requests()[0].contains(r#""count":3"#),
            "{:?}",
            fake.requests()
        );
    }

    #[test]
    fn a_backlog_that_crosses_the_limit_is_dropped_not_retried() {
        let fake = FakeService::answering(vec![(429, "would cross"), (204, "")]);
        let (_root, store) = private_store();
        store.set_pending(40);
        let mut meter = meter(&fake, store.clone());
        assert_eq!(
            meter.admit_with(&key(), SystemTime::now(), Instant::now()),
            Admission::Allowed
        );
        assert_eq!(store.pending(), 0);
        let requests = fake.requests();
        assert!(requests[0].contains(r#""count":41"#), "{requests:?}");
        assert!(requests[1].contains(r#""count":1"#), "{requests:?}");
    }

    #[test]
    fn offline_past_the_grace_denies() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("port")
            .port();
        let (_root, store) = private_store();
        let now = SystemTime::now();
        store.mark_verified(now - OFFLINE_GRACE - Duration::from_secs(1));
        let mut meter = ToolCallMeter::new(store, Service::at(&format!("http://127.0.0.1:{port}")));
        assert!(matches!(
            meter.admit_with(&key(), now, Instant::now()),
            Admission::Denied(sentence) if sentence.contains("7 days")
        ));
    }

    #[test]
    fn no_saved_key_asks_to_sign_in() {
        let (_root, store) = private_store();
        let fake = FakeService::answering(Vec::new());
        let mut meter = meter(&fake, store);
        // `admit` reads NUDOX_API_KEY; the test lanes never set it.
        if std::env::var(crate::API_KEY_ENV).is_err() {
            assert!(matches!(
                meter.admit(),
                Admission::Denied(sentence) if sentence.contains("Sign in")
            ));
        }
    }
}
