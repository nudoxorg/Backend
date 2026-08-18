#![cfg(feature = "embed")]
//! Scheduler behavior: priority ordering, batch coalescing, role splitting,
//! cancellation. All offline, all under the paused tokio clock.

use std::sync::Arc;

use heart::ContentHash;
use registry::vector::EmbedRole;
use registry::vector::embed::mock::MockEmbedder;
use registry::vector::embed::scheduler::{
    CancelGroup, EmbedHandle, EmbedScheduler, Error, Priority, SchedulerConfig,
};

fn key(label: &str) -> ContentHash {
    ContentHash::of_bytes(label.as_bytes())
}

fn spawn_mock() -> (Arc<MockEmbedder>, EmbedHandle<registry::vector::JinaCodeV2>) {
    let mock = Arc::new(MockEmbedder::new());
    let handle = EmbedScheduler::spawn(Arc::clone(&mock), SchedulerConfig::default());
    (mock, handle)
}

/// Jobs submitted within the coalescing window run as one batch, one call.
#[tokio::test(start_paused = true)]
async fn same_priority_jobs_coalesce_into_one_batch() {
    let (mock, handle) = spawn_mock();

    let jobs: Vec<_> = (0..5)
        .map(|i| {
            let handle = handle.clone();
            tokio::spawn(async move {
                handle
                    .embed(
                        key(&format!("k{i}")),
                        format!("text {i}"),
                        EmbedRole::Document,
                        Priority::Background,
                        CancelGroup::new(),
                    )
                    .await
            })
        })
        .collect();

    for job in jobs {
        job.await.expect("join").expect("embed");
    }

    assert_eq!(
        mock.call_count(),
        1,
        "coalesced into a single embed_batch call"
    );
    assert_eq!(mock.batches()[0].len(), 5);
}

/// Interactive jobs that arrive during the coalescing window are served
/// before earlier-arrived background jobs.
#[tokio::test(start_paused = true)]
async fn interactive_overtakes_background() {
    let (mock, handle) = spawn_mock();

    let submit = |text: &str, priority: Priority| {
        let handle = handle.clone();
        let text = text.to_owned();
        tokio::spawn(async move {
            handle
                .embed(
                    key(&text),
                    text,
                    EmbedRole::Document,
                    priority,
                    CancelGroup::new(),
                )
                .await
        })
    };

    // Arrival order: background first, interactive second — both inside the
    // same coalescing window.
    let bg1 = submit("bg one", Priority::Background);
    let bg2 = submit("bg two", Priority::Background);
    tokio::task::yield_now().await;
    let hot1 = submit("hot one", Priority::Interactive);
    let hot2 = submit("hot two", Priority::Interactive);

    for job in [bg1, bg2, hot1, hot2] {
        job.await.expect("join").expect("embed");
    }

    let batches = mock.batches();
    assert_eq!(batches.len(), 2, "one batch per priority");
    assert_eq!(
        batches[0],
        vec!["hot one", "hot two"],
        "interactive ran first"
    );
    assert_eq!(batches[1], vec!["bg one", "bg two"]);
}

/// Batches never mix roles: same priority, different role → separate calls.
#[tokio::test(start_paused = true)]
async fn batches_split_by_role() {
    let (mock, handle) = spawn_mock();

    let submit = |text: &str, role: EmbedRole| {
        let handle = handle.clone();
        let text = text.to_owned();
        tokio::spawn(async move {
            handle
                .embed(
                    key(&text),
                    text,
                    role,
                    Priority::Background,
                    CancelGroup::new(),
                )
                .await
        })
    };

    let d1 = submit("doc a", EmbedRole::Document);
    let d2 = submit("doc b", EmbedRole::Document);
    tokio::task::yield_now().await;
    let q = submit("query", EmbedRole::Query);

    for job in [d1, d2, q] {
        job.await.expect("join").expect("embed");
    }

    let batches = mock.batches();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0], vec!["doc a", "doc b"]);
    assert_eq!(batches[1], vec!["query"]);
}

/// A batch larger than max_batch is split at the cap.
#[tokio::test(start_paused = true)]
async fn oversized_burst_splits_at_max_batch() {
    let mock = Arc::new(MockEmbedder::new());
    let config = SchedulerConfig {
        max_batch: 4,
        ..SchedulerConfig::default()
    };
    let handle = EmbedScheduler::spawn(Arc::clone(&mock), config);

    let jobs: Vec<_> = (0..10)
        .map(|i| {
            let handle = handle.clone();
            tokio::spawn(async move {
                handle
                    .embed(
                        key(&format!("k{i}")),
                        format!("t{i}"),
                        EmbedRole::Document,
                        Priority::Background,
                        CancelGroup::new(),
                    )
                    .await
            })
        })
        .collect();
    for job in jobs {
        job.await.expect("join").expect("embed");
    }

    let sizes: Vec<usize> = mock.batches().iter().map(Vec::len).collect();
    assert!(
        sizes.iter().all(|&len| len <= 4),
        "no batch above the cap: {sizes:?}"
    );
    assert_eq!(sizes.iter().sum::<usize>(), 10);
}

/// Cancelled job groups are dropped before inference; callers observe
/// `Cancelled`, the model is never invoked.
#[tokio::test(start_paused = true)]
async fn cancelled_group_never_reaches_model() {
    let (mock, handle) = spawn_mock();

    let cancel = CancelGroup::new();
    cancel.cancel();

    let result = handle
        .embed(
            key("k"),
            "text".into(),
            EmbedRole::Document,
            Priority::Background,
            cancel,
        )
        .await;

    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(mock.call_count(), 0);
}
