//! Pure ordering tests for the priority-aware dequeue (Daemon Phase 5).
//!
//! These exercise [`registry::queue::runnable_order`] + [`registry::queue::Priority`]
//! — the Rust mirror of the SQL `ORDER BY priority DESC, enqueued_at ASC` clause
//! inside the `SKIP LOCKED` dequeue select — so the scheduling invariant is
//! verified **without a live postgres**. The end-to-end DB ordering is covered by
//! the (postgres-gated) queue integration tests.

use registry::queue::{runnable_order, Priority};

#[test]
fn priority_default_is_normal_zero() {
	assert_eq!(Priority::default(), Priority::NORMAL);
	assert_eq!(Priority::default().get(), 0);
	assert_eq!(Priority::from(7).get(), 7);
	assert_eq!(Priority::new(-3).get(), -3);
}

#[test]
fn priority_orders_naturally_ascending() {
	assert!(Priority::new(1) < Priority::new(2));
	assert!(Priority::new(-5) < Priority::new(0));
}

#[test]
fn higher_priority_is_claimed_first() {
	let high = (Priority::new(10), 1_000i64);
	let low = (Priority::new(0), 10i64); // older, but lower priority
	assert_eq!(runnable_order(high, low), std::cmp::Ordering::Less);
	assert_eq!(runnable_order(low, high), std::cmp::Ordering::Greater);
}

#[test]
fn equal_priority_breaks_ties_fifo() {
	let older = (Priority::new(5), 100i64);
	let newer = (Priority::new(5), 200i64);
	assert_eq!(runnable_order(older, newer), std::cmp::Ordering::Less);
	assert_eq!(runnable_order(newer, older), std::cmp::Ordering::Greater);
	assert_eq!(runnable_order(older, older), std::cmp::Ordering::Equal);
}

#[test]
fn sorting_a_batch_matches_dequeue_order() {
	// A shuffled batch sorted by `runnable_order` must come out
	// highest-priority-first, FIFO within a priority.
	let mut batch = vec![
		(Priority::new(0), 50i64),
		(Priority::new(10), 300i64),
		(Priority::new(10), 100i64),
		(Priority::new(5), 10i64),
	];
	batch.sort_by(|&a, &b| runnable_order(a, b));
	assert_eq!(
		batch,
		vec![
			(Priority::new(10), 100i64), // highest prio, oldest of the two
			(Priority::new(10), 300i64),
			(Priority::new(5), 10i64),
			(Priority::new(0), 50i64),
		]
	);
}
