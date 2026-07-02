//! Real tests for the cross-cutting primitives every subsystem leans on:
//! type-tagged ids, the scored wrapper, versioned objects, derived progress, and
//! the retrying sink.

use heart::{
	Id, Scored, Versioned,
	package::PackageVersion,
	progress::{JobProgress, Percent, Progressive},
	score::Score,
};

struct A;
struct B;

/// `Id<T>` compares and hashes by its inner UUID; the phantom tag doesn't affect
/// identity, and re-tagging preserves the UUID.
#[test]
fn id_equality_ignores_the_type_tag() {
	let uuid = uuid::Uuid::from_u128(42);
	let a1 = Id::<A>::from_uuid(uuid);
	let a2 = Id::<A>::from_uuid(uuid);
	assert_eq!(a1, a2);

	// Re-tag to a different phantom type; the underlying UUID is unchanged.
	let b: Id<B> = a1.cast();
	assert_eq!(b.as_uuid(), a1.as_uuid());
}

/// `Scored<T>` always carries a value and a (finite) score — ranking is total.
#[test]
fn scored_pairs_value_with_score() {
	let s = Scored::new("axum", Score::try_new(0.9).unwrap());
	assert_eq!(s.value, "axum");
	assert_eq!(s.score, Score::try_new(0.9).unwrap());

	// Mapping preserves the score.
	let mapped = s.map(|v| v.len());
	assert_eq!(mapped.value, 4);
	assert_eq!(mapped.score, Score::try_new(0.9).unwrap());
}

/// `Versioned<T>` pairs an object with its (ecosystem-typed) version.
#[test]
fn versioned_pairs_object_with_version() {
	let v = Versioned::new(PackageVersion::Cargo(semver::Version::new(1, 2, 3)), "payload");
	assert_eq!(*v.get(), "payload");
	assert!(matches!(v.version(), PackageVersion::Cargo(_)));
	assert_eq!(v.into_inner(), "payload");
}

/// `Progressive::is_complete` derives from the persisted state: `Stored` is 100%,
/// an in-flight phase is not complete.
#[test]
fn progressive_completes_from_state() {
	use heart::content::ContentHash;
	use heart::lifecycle::{Phase, ResolutionState};

	let stored = JobProgress {
		state: ResolutionState::Stored { hash: ContentHash::of_bytes(b"x") },
		phase_fraction: Percent::try_new(0).unwrap(),
	};
	assert!(stored.is_complete());
	assert_eq!(stored.overall(), Percent::try_new(100).unwrap());

	let mid = JobProgress {
		state: ResolutionState::Progressing(Phase::Acquiring),
		phase_fraction: Percent::try_new(50).unwrap(),
	};
	assert!(!mid.is_complete());
}

/// `Sink::deliver` retries transient failures per the backoff, and surfaces a
/// non-retryable error immediately.
#[tokio::test]
async fn sink_deliver_retries_transient_failures() {
	use std::sync::atomic::{AtomicU32, Ordering};

	use heart::{Retryable, Sink};

	#[derive(Debug)]
	struct TestErr {
		retryable: bool,
	}
	impl std::fmt::Display for TestErr {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "test error") }
	}
	impl std::error::Error for TestErr {}
	impl Retryable for TestErr {
		fn is_retryable(&self) -> bool { self.retryable }
	}

	// Fails `fails_left` times (transiently) then succeeds.
	struct Flaky {
		fails_left: AtomicU32,
		attempts: AtomicU32,
	}
	impl Sink for Flaky {
		type Item = ();
		type Error = TestErr;
		async fn upload(&self, _item: ()) -> Result<(), TestErr> {
			self.attempts.fetch_add(1, Ordering::SeqCst);
			if self
				.fails_left
				.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
				.is_ok()
			{
				Result::Err(TestErr { retryable: true })
			} else {
				Ok(())
			}
		}
	}

	let flaky = Flaky { fails_left: AtomicU32::new(2), attempts: AtomicU32::new(0) };
	assert!(flaky.deliver(()).await.is_ok());
	assert_eq!(flaky.attempts.load(Ordering::SeqCst), 3, "2 transient failures then success");

	// A non-retryable failure is surfaced on the first attempt.
	struct Fatal {
		attempts: AtomicU32,
	}
	impl Sink for Fatal {
		type Item = ();
		type Error = TestErr;
		async fn upload(&self, _item: ()) -> Result<(), TestErr> {
			self.attempts.fetch_add(1, Ordering::SeqCst);
			Result::Err(TestErr { retryable: false })
		}
	}
	let fatal = Fatal { attempts: AtomicU32::new(0) };
	assert!(fatal.deliver(()).await.is_err());
	assert_eq!(fatal.attempts.load(Ordering::SeqCst), 1, "non-retryable: no retries");
}
