use std::time::Duration;

use tracing::warn;

/// Returns true if `message` contains any of the well-known transient-failure
/// patterns (connection resets, timeouts, service-unavailable responses).
///
/// This is the single source of truth for the retry keyword set — previously
/// duplicated in `is_retryable_sync_error_message` and
/// `is_retryable_terminus_error_message`.
pub(crate) fn is_transient_message(message: &str) -> bool {
	let n = message.to_ascii_lowercase();
	n.contains("backend db connection error")
		|| n.contains("error sending request for url")
		|| n.contains("connection reset")
		|| n.contains("connection refused")
		|| n.contains("timed out")
		|| n.contains("timeout")
		|| n.contains("temporarily unavailable")
		|| n.contains("503 service unavailable")
}

/// Errors that can be classified as transient (safe to retry) vs. permanent.
pub trait Transient {
	fn is_transient(&self) -> bool;
}

impl Transient for String {
	fn is_transient(&self) -> bool { is_transient_message(self) }
}

/// Retry `f` with linear back-off, stopping when it succeeds or the error is
/// permanent / the attempt budget is exhausted.
///
/// `base_delay` is multiplied by the attempt number: attempt 1 → `base_delay`,
/// attempt 2 → `2 × base_delay`, …
pub async fn with_backoff<T, E, Fut>(
	max_attempts: usize,
	base_delay: Duration,
	mut f: impl FnMut(usize) -> Fut,
) -> Result<T, E>
where
	E: Transient + std::fmt::Display,
	Fut: std::future::Future<Output = Result<T, E>>,
{
	let mut attempt = 0;
	loop {
		attempt += 1;
		match f(attempt).await {
			Ok(v) => return Ok(v),
			Err(e) if attempt < max_attempts && e.is_transient() => {
				let delay = base_delay * attempt as u32;
				warn!(
					attempt,
					max_attempts,
					delay_secs = delay.as_secs(),
					error = %e,
					"transient error, retrying"
				);
				tokio::time::sleep(delay).await;
			}
			Err(e) => return Err(e),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn recognises_transient_messages() {
		let cases = [
			"Backend DB connection error",
			"connection reset by peer",
			"connection refused",
			"request timed out",
			"operation timeout",
			"service temporarily unavailable",
			"503 Service Unavailable",
		];
		for msg in cases {
			assert!(is_transient_message(msg), "should be transient: {msg}");
		}
	}

	#[test]
	fn rejects_permanent_messages() {
		let cases = ["version 1.0.0 not found", "invalid configuration", "parse error"];
		for msg in cases {
			assert!(!is_transient_message(msg), "should not be transient: {msg}");
		}
	}

	#[tokio::test]
	async fn with_backoff_succeeds_on_first_try() {
		let result: Result<u32, String> =
			with_backoff(3, Duration::from_millis(1), |_| async { Ok(42u32) }).await;
		assert_eq!(result.unwrap(), 42);
	}

	#[tokio::test]
	async fn with_backoff_retries_transient_and_succeeds() {
		let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let count2 = count.clone();
		let result: Result<u32, String> = with_backoff(3, Duration::from_millis(1), move |_| {
			let c = count2.clone();
			async move {
				let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
				if n < 2 { Err("connection reset".to_string()) } else { Ok(n as u32) }
			}
		})
		.await;
		assert!(result.is_ok());
		assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 3);
	}

	#[tokio::test]
	async fn with_backoff_does_not_retry_permanent() {
		let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let count2 = count.clone();
		let result: Result<u32, String> = with_backoff(3, Duration::from_millis(1), move |_| {
			let c = count2.clone();
			async move {
				c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
				Err("version 1.0.0 not found".to_string())
			}
		})
		.await;
		assert!(result.is_err());
		assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
	}
}
