#![cfg(feature = "embed")]
//! EmbedGate lifecycle: idle unload, lazy reload, session serialization.
//! Paused-clock tests — the watchdog runs on `tokio::time`.

use std::sync::Arc;
use std::time::Duration;

use registry::vector::embed::gate::{EmbedGate, GateConfig, LoadState};
use registry::vector::embed::mock::MockEmbedder;

fn mock_gate(unload_idle: Duration) -> Arc<EmbedGate<MockEmbedder>> {
    EmbedGate::new(
        GateConfig {
            max_sessions: 1,
            unload_idle,
        },
        Box::new(|| Box::pin(async { Ok(MockEmbedder::new()) })),
    )
}

#[tokio::test(start_paused = true)]
async fn starts_idle_loads_on_first_acquire() {
    let gate = mock_gate(Duration::from_mins(2));
    assert_eq!(gate.state(), LoadState::Idle);
    assert_eq!(gate.load_count(), 0);

    let guard = gate.acquire().await.expect("acquire");
    assert_eq!(gate.state(), LoadState::Loaded);
    assert_eq!(gate.load_count(), 1);
    drop(guard);
}

#[tokio::test(start_paused = true)]
async fn unloads_after_idle_and_reloads_on_demand() {
    let gate = mock_gate(Duration::from_mins(2));

    drop(gate.acquire().await.expect("first acquire"));
    assert_eq!(gate.state(), LoadState::Loaded);

    // Idle past the threshold: the watchdog (ticking every idle/4) unloads.
    tokio::time::sleep(Duration::from_secs(200)).await;
    assert_eq!(
        gate.state(),
        LoadState::Idle,
        "embedder unloaded after idle"
    );

    // Next use lazily reloads via the factory.
    let guard = gate.acquire().await.expect("reacquire");
    assert_eq!(gate.state(), LoadState::Loaded);
    assert_eq!(gate.load_count(), 2, "factory invoked again on reload");
    drop(guard);
}

#[tokio::test(start_paused = true)]
async fn stays_loaded_while_recently_used() {
    let gate = mock_gate(Duration::from_mins(2));

    // Touch every 60 s — never idle long enough to unload.
    for _ in 0..5 {
        drop(gate.acquire().await.expect("acquire"));
        tokio::time::sleep(Duration::from_mins(1)).await;
    }
    assert_eq!(gate.state(), LoadState::Loaded);
    assert_eq!(gate.load_count(), 1, "never reloaded");
}

#[tokio::test(start_paused = true)]
async fn held_guard_blocks_unload() {
    let gate = mock_gate(Duration::from_mins(2));

    let guard = gate.acquire().await.expect("acquire");
    tokio::time::sleep(Duration::from_secs(500)).await;
    assert_eq!(
        gate.state(),
        LoadState::Loaded,
        "in-use embedder is never unloaded"
    );
    drop(guard);
}

#[tokio::test(start_paused = true)]
async fn single_session_serializes_acquires() {
    let gate = mock_gate(Duration::from_mins(2));

    let guard = gate.acquire().await.expect("first session");
    let second = tokio::time::timeout(Duration::from_millis(100), gate.acquire()).await;
    assert!(
        second.is_err(),
        "second acquire must wait while a session is held"
    );

    drop(guard);
    let guard = tokio::time::timeout(Duration::from_millis(100), gate.acquire())
        .await
        .expect("acquire proceeds after release")
        .expect("acquire");
    drop(guard);
}

#[tokio::test(start_paused = true)]
async fn manual_unload_then_reload() {
    let gate = mock_gate(Duration::from_mins(2));
    drop(gate.acquire().await.expect("acquire"));

    gate.unload().await;
    assert_eq!(gate.state(), LoadState::Idle);

    drop(gate.acquire().await.expect("reacquire"));
    assert_eq!(gate.load_count(), 2);
}
