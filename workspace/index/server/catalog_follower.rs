//! The catalog-follower driver: poll upstream feeds, register events, persist
//! cursors.

#[allow(unused_imports)]
use crate::server::registry;
use std::sync::Arc;

use crate::ecosystem::PackageNameExt as _;
use registry::vector::EmbeddingModel;

use crate::server::Server;

/// Cursor filename pattern: `catalog-cursor-{lang}.json`, placed next to the
/// tantivy watermark files so a `data_directory` wipe resets both.
fn cursor_path(
    data_dir: &std::path::Path,
    language: crate::ecosystem::Language,
) -> std::path::PathBuf {
    data_dir.join(format!("catalog-cursor-{language}.json"))
}

/// Load a persisted cursor from disk; returns `CatalogCursor::zero()` on any
/// error (missing file, corrupt JSON).
fn load_cursor(path: &std::path::Path) -> registry::upstream::CatalogCursor {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "corrupt catalog cursor; restarting from zero");
                registry::upstream::CatalogCursor::zero()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            registry::upstream::CatalogCursor::zero()
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read catalog cursor; restarting from zero");
            registry::upstream::CatalogCursor::zero()
        }
    }
}

/// Persist a cursor atomically (tmp-write + rename, matching the tantivy
/// watermark pattern so they are both crash-safe).
fn persist_cursor(path: &std::path::Path, cursor: &registry::upstream::CatalogCursor) {
    let Ok(bytes) = serde_json::to_vec(cursor) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        tracing::warn!(path = %tmp.display(), error = %e, "failed to write catalog cursor tmp");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        tracing::warn!(path = %path.display(), error = %e, "failed to persist catalog cursor");
    }
}

/// Drive one `CatalogFollower` as a supervised background task.
///
/// Per-follower discipline:
/// 1. Load the durable cursor from disk (zero on first run or corrupt file).
/// 2. Loop: a. **Backpressure check**: if the definitive source's queue depth
///    exceeds `mirror.queue_ceiling`, sleep `poll_interval` and retry. This
///    prevents the catalog follower from outrunning the compile workers. b.
///    Poll the follower for the next batch. c. For each event in the batch,
///    call the idempotent registration entry point (same as `POST /packages`).
///    d. **Commit** — persist the cursor to disk only after ALL events in the
///    batch have been registered. A crash between (c) and (d) re-delivers the
///    whole batch on restart; the registration call is idempotent. e. If
///    `exhausted`, sleep `poll_interval` before the next poll. f. On error, log
///    + sleep + retry (exponential is NOT used for catalog followers — the
///    poll_interval is already the correct cadence).
///
/// The task never returns under normal operation; it is torn down by abort at
/// the next await point during shutdown.
pub(crate) async fn catalog_follower_worker<M: EmbeddingModel>(
    server: Arc<Server<M>>,
    follower: Box<dyn registry::upstream::CatalogFollower>,
) {
    use crate::server::authz::WriteCap;
    use heart::{Language, PackageVersion, RegistryOrigin};
    use registry::{
        package::{Coordinates, PackageName},
        upstream::CatalogEvent,
    };

    let lang = follower.language();
    let interval = server.config().limits.poll_interval;
    let ceiling = server.config().mirror.queue_ceiling;

    // Cursor lives in the definitive source's data directory, next to the
    // tantivy watermarks.
    let data_dir = server.config().definitive.data_directory();
    let cursor_file = cursor_path(&data_dir, lang);

    let upstream_origin = match lang {
        Language::Rust => RegistryOrigin::CratesIo,
        Language::CSharp => RegistryOrigin::NuGet,
        Language::Typescript => RegistryOrigin::NpmPublic,
        Language::Python => RegistryOrigin::PyPi,
        Language::Go => RegistryOrigin::GoProxy,
        Language::Java => RegistryOrigin::MavenCentral,
        // `cpp` followers are git-native (RL-1); there is no upstream registry.
        Language::Cpp => RegistryOrigin::Git,
    };

    let client = registry::upstream::UpstreamClient::new();
    let mut cursor = load_cursor(&cursor_file);

    tracing::info!(%lang, cursor_is_zero = cursor.is_zero(), "catalog follower started");

    loop {
        // ── Backpressure check ────────────────────────────────────────────────
        let depth = server.base().queue.pending_count().await.unwrap_or(0);
        if depth as usize >= ceiling {
            tracing::debug!(
                %lang,
                depth,
                ceiling,
                "catalog follower paused: indexing queue above ceiling"
            );
            metrics::gauge!("catalog_follower_paused", "language" => lang.to_string()).set(1.0);
            tokio::time::sleep(interval).await;
            continue;
        }
        metrics::gauge!("catalog_follower_paused", "language" => lang.to_string()).set(0.0);

        // ── Poll the follower ─────────────────────────────────────────────────
        let batch = match follower.poll(&client, &cursor).await {
            Ok(b) => b,
            Err(error) => {
                tracing::warn!(%lang, error = %error, "catalog follower poll failed; backing off");
                metrics::counter!("catalog_follower_errors", "language" => lang.to_string())
                    .increment(1);
                tokio::time::sleep(interval).await;
                continue;
            }
        };

        // ── Register each event ───────────────────────────────────────────────
        let cap = WriteCap::system();
        let mut registered = 0usize;
        let mut failed = 0usize;

        for event in &batch.events {
            // Build typed coordinates from the raw name/version strings.
            let name = match PackageName::new(lang, event.name()) {
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!(%lang, name = event.name(), error = %e, "catalog event name invalid; skipping");
                    continue;
                }
            };
            let version = match PackageVersion::try_from((lang, event.version())) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(%lang, name = event.name(), version = event.version(), error = %e, "catalog event version invalid; skipping");
                    continue;
                }
            };
            let coords = Coordinates {
                origin: upstream_origin.clone(),
                name,
                version,
            };

            match event {
                CatalogEvent::Published { .. } => {
                    // Idempotent: ensures the package is known and enqueued.
                    // Dependency names from the feed land on the provisional
                    // record so the dependents sweep can run before compile.
                    match server
                        .ensure_initialized_with(&cap, &coords, event.dependencies())
                        .await
                    {
                        Ok(_) => {
                            registered += 1;
                        }
                        Err(e) => {
                            tracing::warn!(%lang, name = event.name(), error = %e, "catalog published event registration failed");
                            failed += 1;
                        }
                    }
                }
                CatalogEvent::Withdrawn { .. } => {
                    // The package may not yet be indexed; `ensure_initialized` is
                    // idempotent and safe to call here. After registration, emit
                    // Delete outbox intents so the derived stores remove visibility.
                    match server.ensure_initialized(&cap, &coords).await {
                        Ok(initialized) => {
                            // Emit Delete intents for all sinks. Using a zero-hash
                            // sentinel for the generation: withdrawals don't produce a
                            // new blob generation; the important thing is that the outbox
                            // consumer removes the search projection. We use the package
                            // id as a stable seed for the generation sentinel to avoid
                            // collision with real content hashes.
                            let sentinel = heart::content::ContentHash::of_bytes(
                                &initialized.package.as_uuid().to_bytes_le(),
                            );
                            if let Err(e) = server
                                .base()
                                .outbox
                                .emit_withdraw_intents_for_version(initialized.package, sentinel)
                                .await
                            {
                                tracing::warn!(%lang, name = event.name(), error = %e, "catalog withdraw: outbox tombstones failed");
                            }
                            registered += 1;
                        }
                        Err(e) => {
                            tracing::warn!(%lang, name = event.name(), error = %e, "catalog withdrawn event registration failed");
                            failed += 1;
                        }
                    }
                }
            }
        }

        if registered > 0 || failed > 0 {
            metrics::counter!("catalog_events_registered", "language" => lang.to_string())
                .increment(registered as u64);
            if failed > 0 {
                metrics::counter!("catalog_events_failed", "language" => lang.to_string())
                    .increment(failed as u64);
            }
            tracing::info!(%lang, registered, failed, "catalog batch processed");
        }

        // ── Commit cursor (only after all events registered) ──────────────────
        // This is the crash-safe commit gate: a process crash between event
        // registration and cursor persistence re-delivers the whole batch on
        // restart. Registration is idempotent (upsert), so re-delivery is safe.
        if !batch.events.is_empty() || !matches!(&batch.next.0, serde_json::Value::Null) {
            cursor = batch.next;
            persist_cursor(&cursor_file, &cursor);
        }

        // ── Backoff if exhausted ──────────────────────────────────────────────
        if batch.exhausted {
            tracing::debug!(%lang, "catalog follower exhausted; sleeping");
            tokio::time::sleep(interval).await;
        }
    }
}

/// Drive one [`crate::ingest::osv::OsvFollower`] per mirrored ecosystem.
///
/// Every bucket shares one watermark directory. Feed ids differ (`osv-npm`
/// versus `osv-crates.io`), and polls run one after another so a single
/// `feeds.json` rewrite is the only writer. A poll runs on the blocking pool
/// because the feed client is synchronous. Each cursor advances only after
/// `FollowerDriver` commits that bucket's batch.
pub(crate) async fn osv_follower_worker<M: EmbeddingModel>(server: Arc<Server<M>>) {
    let interval = server.config().limits.poll_interval;
    let buckets = crate::ingest::osv::buckets_for_follow(&server.config().mirror.follow);
    if buckets.is_empty() {
        return;
    }
    let data_dir = server.config().definitive.data_directory();
    let watermarks =
        match crate::ingest::watermark::FileWatermarkStore::open(data_dir.join("osv-watermarks")) {
            Ok(store) => std::sync::Arc::new(store),
            Err(error) => {
                tracing::warn!(error = %error, "osv follower watermark store failed to open");
                return;
            }
        };
    let writer = std::sync::Arc::clone(server.base().global_store.writer());
    tracing::info!(?buckets, "osv follower started");
    loop {
        for bucket in &buckets {
            let writer = std::sync::Arc::clone(&writer);
            let watermarks = std::sync::Arc::clone(&watermarks);
            let bucket_name = bucket.to_owned();
            let joined = tokio::task::spawn_blocking(move || {
                let follower = crate::ingest::osv::OsvFollower::for_bucket(
                    crate::ingest::HttpTransport::new(),
                    &bucket_name,
                );
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(0);
                crate::ingest::FollowerDriver::new(writer.as_ref(), watermarks.as_ref())
                    .drive_once(&follower, now)
            })
            .await;
            match joined {
                Ok(Ok(outcome)) => tracing::debug!(bucket, ?outcome, "osv follower poll"),
                Ok(Err(error)) => {
                    tracing::warn!(bucket, error = %error, "osv follower poll failed")
                }
                Err(error) => tracing::warn!(bucket, error = %error, "osv follower task failed"),
            }
        }
        tokio::time::sleep(interval).await;
    }
}
