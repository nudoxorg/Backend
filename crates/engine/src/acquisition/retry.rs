use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use super::identity::now_millis;
use super::outcome::{CircuitOpen, NegativeFactKind};

/// Error class used by retry and breaker policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryClass {
    /// Network timeout, 5xx, or rate limit.
    Retryable,
    /// Permanent source absence or explicit rejection.
    Permanent,
    /// Malformed protocol response.
    Protocol,
    /// Local policy rejection.
    Policy,
    /// Integrity failure; never converted into a negative fact.
    Integrity,
}

/// Error classification retained with an attempt journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptFailure {
    /// Request timed out.
    Timeout,
    /// Server requested a retry, optionally with Retry-After milliseconds.
    RateLimited { retry_after_millis: Option<u64> },
    /// Source returned an unavailable response.
    Unavailable,
    /// Source returned an explicit negative fact.
    NotFound,
    /// Response was malformed.
    Malformed,
    /// Policy denied the request.
    Policy,
    /// Integrity verification failed.
    Integrity,
}

impl AttemptFailure {
    /// Classifies a failure without collapsing transient errors into absence.
    #[must_use]
    pub const fn class(self) -> RetryClass {
        match self {
            Self::Timeout | Self::RateLimited { .. } | Self::Unavailable => RetryClass::Retryable,
            Self::NotFound => RetryClass::Permanent,
            Self::Malformed => RetryClass::Protocol,
            Self::Policy => RetryClass::Policy,
            Self::Integrity => RetryClass::Integrity,
        }
    }

    /// Converts only a definitive source absence into a negative fact kind.
    /// Every transient, protocol, policy, or integrity failure returns `None`.
    #[must_use]
    pub const fn negative_kind(self) -> Option<NegativeFactKind> {
        match self {
            Self::NotFound => Some(NegativeFactKind::NotFound),
            Self::Timeout
            | Self::RateLimited { .. }
            | Self::Unavailable
            | Self::Malformed
            | Self::Policy
            | Self::Integrity => None,
        }
    }
}

/// Full-jitter exponential retry policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    /// Initial backoff cap.
    pub base: Duration,
    /// Maximum backoff cap.
    pub maximum: Duration,
    /// Maximum retry attempts.
    pub max_attempts: u32,
    seed: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_millis(50),
            maximum: Duration::from_secs(30),
            max_attempts: 5,
            seed: now_millis() ^ (std::process::id() as u64),
        }
    }
}

impl RetryPolicy {
    /// Creates a deterministic policy useful for replay tests.
    #[must_use]
    pub const fn with_seed(
        base: Duration,
        maximum: Duration,
        max_attempts: u32,
        seed: u64,
    ) -> Self {
        Self {
            base,
            maximum,
            max_attempts,
            seed,
        }
    }

    /// Computes a full-jitter delay and clamps a server Retry-After value.
    #[must_use]
    pub fn delay(self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let exponent = attempt.min(31);
        let cap = self
            .base
            .checked_mul(1_u32 << exponent)
            .unwrap_or(self.maximum)
            .min(self.maximum);
        let jitter = if cap.is_zero() {
            Duration::ZERO
        } else {
            let mut value = self.seed ^ u64::from(attempt).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            value ^= value << 7;
            value ^= value >> 9;
            let nanos = cap.as_nanos().min(u128::from(u64::MAX)) as u64;
            Duration::from_nanos(value % nanos.saturating_add(1))
        };
        retry_after.map_or(jitter, |requested| requested.min(self.maximum).max(jitter))
    }

    /// Returns whether an attempt may be retried.
    #[must_use]
    pub const fn retryable(self, attempt: u32, failure: AttemptFailure) -> bool {
        attempt < self.max_attempts && matches!(failure.class(), RetryClass::Retryable)
    }
}

/// Persisted circuit state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CircuitState {
    /// Requests flow normally.
    Closed,
    /// Requests fail fast until `until_millis`.
    Open { until_millis: u64 },
    /// Exactly one probe may run.
    HalfOpen,
}

#[derive(Clone, Copy, Debug)]
struct CircuitPersisted {
    state: CircuitState,
    failures: u32,
    probe: bool,
}

/// Persistent breaker with one half-open probe.
#[derive(Clone, Debug)]
pub struct CircuitBreaker {
    state: Arc<Mutex<CircuitPersisted>>,
    threshold: u32,
    cool_down: Duration,
    path: Option<Arc<PathBuf>>,
}

impl CircuitBreaker {
    /// Creates a process-local circuit.
    #[must_use]
    pub fn new(threshold: u32, cool_down: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(CircuitPersisted {
                state: CircuitState::Closed,
                failures: 0,
                probe: false,
            })),
            threshold: threshold.max(1),
            cool_down,
            path: None,
        }
    }

    /// Opens a breaker backed by a small durable state file.
    pub fn open_persisted(
        path: impl Into<PathBuf>,
        threshold: u32,
        cool_down: Duration,
    ) -> io::Result<Self> {
        let path = path.into();
        let breaker = Self {
            state: Arc::new(Mutex::new(CircuitPersisted {
                state: CircuitState::Closed,
                failures: 0,
                probe: false,
            })),
            threshold: threshold.max(1),
            cool_down,
            path: Some(Arc::new(path)),
        };
        breaker.load()?;
        Ok(breaker)
    }

    fn load(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let Ok(text) = fs::read_to_string(path.as_ref()) else {
            return Ok(());
        };
        let mut fields = text.split(':');
        let state = match fields.next() {
            Some("closed") => CircuitState::Closed,
            Some("open") => CircuitState::Open {
                until_millis: fields
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
            },
            Some("half") => CircuitState::HalfOpen,
            _ => return Ok(()),
        };
        let failures = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut current = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        current.state = state;
        current.failures = failures;
        current.probe = false;
        Ok(())
    }

    fn persist(&self, value: CircuitPersisted) {
        let Some(path) = &self.path else { return };
        let text = match value.state {
            CircuitState::Closed => format!("closed:{}", value.failures),
            CircuitState::Open { until_millis } => {
                format!("open:{until_millis}:{}", value.failures)
            }
            CircuitState::HalfOpen => format!("half:{}", value.failures),
        };
        let _ = fs::write(path.as_ref(), text);
    }

    /// Admits a request or returns a typed open observation. One caller owns
    /// the half-open probe; all others receive `false`.
    pub fn allow(&self, now_millis: u64) -> Result<CircuitPermit, CircuitOpen> {
        let mut value = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match value.state {
            CircuitState::Closed => Ok(CircuitPermit {
                state: Arc::clone(&self.state),
                path: self.path.clone(),
                probe: false,
            }),
            CircuitState::Open { until_millis } if now_millis < until_millis => {
                Err(CircuitOpen { until_millis })
            }
            CircuitState::Open { .. } => {
                if value.probe {
                    return Err(CircuitOpen {
                        until_millis: now_millis.saturating_add(self.cool_down.as_millis() as u64),
                    });
                }
                value.state = CircuitState::HalfOpen;
                value.probe = true;
                self.persist(*value);
                Ok(CircuitPermit {
                    state: Arc::clone(&self.state),
                    path: self.path.clone(),
                    probe: true,
                })
            }
            CircuitState::HalfOpen => Err(CircuitOpen {
                until_millis: now_millis.saturating_add(self.cool_down.as_millis() as u64),
            }),
        }
    }

    /// Records a successful request and closes the breaker.
    pub fn success(&self) {
        let mut value = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        value.state = CircuitState::Closed;
        value.failures = 0;
        value.probe = false;
        self.persist(*value);
    }

    /// Records a retryable failure and opens once the threshold is reached.
    pub fn failure(&self, now_millis: u64) {
        let mut value = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        value.failures = value.failures.saturating_add(1);
        value.probe = false;
        if value.failures >= self.threshold {
            value.state = CircuitState::Open {
                until_millis: now_millis.saturating_add(self.cool_down.as_millis() as u64),
            };
        }
        self.persist(*value);
    }

    /// Returns a compact state snapshot.
    #[must_use]
    pub fn state(&self) -> CircuitState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .state
    }
}

/// Lease that releases a single half-open probe on drop if still owned.
pub struct CircuitPermit {
    state: Arc<Mutex<CircuitPersisted>>,
    path: Option<Arc<PathBuf>>,
    probe: bool,
}

impl Drop for CircuitPermit {
    fn drop(&mut self) {
        if self.probe {
            let mut value = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            value.probe = false;
            if value.state == CircuitState::HalfOpen {
                value.state = CircuitState::Open {
                    until_millis: now_millis(),
                };
            }
            if let Some(path) = &self.path {
                let text = match value.state {
                    CircuitState::Closed => format!("closed:{}", value.failures),
                    CircuitState::Open { until_millis } => {
                        format!("open:{until_millis}:{}", value.failures)
                    }
                    CircuitState::HalfOpen => format!("half:{}", value.failures),
                };
                let _ = fs::write(path.as_ref(), text);
            }
        }
    }
}
