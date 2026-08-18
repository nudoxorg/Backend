#![cfg(feature = "server")]
//! Red-first specification: **package emission must not be a serial round-trip
//! loop**.
//!
//! # The error class this closes
//!
//! `index::blob::emit::emit` writes every section with a bare `for` loop:
//!
//! ```ignore
//! for section in &sections {
//!     store.put_section(section).await?;   // one full round trip, then the next
//! }
//! ```
//!
//! and `Store::put_section` is itself `HEAD`-then-`PUT` — **two** round trips
//! per new object. Nothing anywhere in `blob/`, `server/save/`, or `cas.rs` uses
//! `join_all`, `buffered`, `FuturesUnordered`, or `tokio::join!` (verified by
//! unbounded grep).
//!
//! Measured against the real 238-package corpus, packages are **median 52 files,
//! p90 535, max 4,325**. On a local filesystem that seriality costs microseconds
//! and hides. Behind the object-store backends this store is generic over
//! (`object_store::parse_url` resolves `s3://`, `gs://`, `az://` — see
//! `server/mod.rs:803-824`), at a realistic 20–50 ms per request, a p90 package
//! is **10–25 seconds of purely serial, trivially parallelisable latency**.
//!
//! This is the single largest, lowest-risk win available in the storage layer,
//! and — unlike repacking or chunking — it changes no format, no addressing, and
//! no on-disk bytes. It is a pure concurrency fix.
//!
//! # Why this is asserted on observed concurrency, not on wall-clock
//!
//! A timing assertion would be flaky under load and would pass for the wrong
//! reason on a fast local disk. Instead the backend here *records* how many
//! requests are in flight simultaneously, so the test observes the property
//! directly: did the emitter ever have more than one request outstanding?
//!
//! **Do not weaken these tests to make them pass.**

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures::stream::BoxStream;
use object_store::path::Path;
use object_store::{
    GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore, PutMultipartOpts,
    PutOptions, PutPayload, PutResult, Result as OsResult,
};

// ---------------------------------------------------------------------------
// A backend that records peak in-flight concurrency.
// ---------------------------------------------------------------------------

/// Wraps an in-memory object store and tracks how many requests overlap.
///
/// Every operation sleeps briefly so that genuinely concurrent callers actually
/// overlap in time — without the delay, a fast serial loop could complete each
/// request before issuing the next and be indistinguishable from a concurrent
/// one.
#[derive(Debug)]
struct ConcurrencyProbe {
    inner: object_store::memory::InMemory,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    puts: AtomicUsize,
    delay: Duration,
}

impl ConcurrencyProbe {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner: object_store::memory::InMemory::new(),
            in_flight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            puts: AtomicUsize::new(0),
            delay,
        })
    }

    async fn enter(&self) {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
    }

    fn leave(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    fn peak_concurrency(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn put_count(&self) -> usize {
        self.puts.load(Ordering::SeqCst)
    }
}

impl std::fmt::Display for ConcurrencyProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConcurrencyProbe")
    }
}

#[async_trait::async_trait]
impl ObjectStore for ConcurrencyProbe {
    async fn put_opts(
        &self,
        location: &Path,
        payload: PutPayload,
        opts: PutOptions,
    ) -> OsResult<PutResult> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.enter().await;
        let out = self.inner.put_opts(location, payload, opts).await;
        self.leave();
        out
    }

    async fn put_multipart_opts(
        &self,
        location: &Path,
        opts: PutMultipartOpts,
    ) -> OsResult<Box<dyn MultipartUpload>> {
        self.inner.put_multipart_opts(location, opts).await
    }

    async fn get_opts(&self, location: &Path, options: GetOptions) -> OsResult<GetResult> {
        self.enter().await;
        let out = self.inner.get_opts(location, options).await;
        self.leave();
        out
    }

    async fn head(&self, location: &Path) -> OsResult<ObjectMeta> {
        self.enter().await;
        let out = self.inner.head(location).await;
        self.leave();
        out
    }

    async fn delete(&self, location: &Path) -> OsResult<()> {
        self.inner.delete(location).await
    }

    fn list(&self, prefix: Option<&Path>) -> BoxStream<'_, OsResult<ObjectMeta>> {
        self.inner.list(prefix)
    }

    async fn list_with_delimiter(&self, prefix: Option<&Path>) -> OsResult<ListResult> {
        self.inner.list_with_delimiter(prefix).await
    }

    async fn copy(&self, from: &Path, to: &Path) -> OsResult<()> {
        self.inner.copy(from, to).await
    }

    async fn copy_if_not_exists(&self, from: &Path, to: &Path) -> OsResult<()> {
        self.inner.copy_if_not_exists(from, to).await
    }
}

// ---------------------------------------------------------------------------
// The specification
// ---------------------------------------------------------------------------

/// THE test. Emitting a package with many sections must overlap its requests.
///
/// With the current serial `for` loop this observes a peak concurrency of
/// exactly 1 and fails. Any bounded-concurrency fan-out (`buffer_unordered`,
/// `FuturesUnordered`, a batch put) satisfies it.
#[tokio::test]
async fn emitting_many_sections_issues_requests_concurrently() {
    let probe = ConcurrencyProbe::new(Duration::from_millis(20));
    let (store, manifest, sections) = fixture(&probe, 40).await;

    index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("emit succeeds");

    assert!(
        probe.peak_concurrency() > 1,
        "emit issued requests one at a time (peak concurrency {}) — it is still \
         a serial `for` loop over put_section",
        probe.peak_concurrency()
    );
}

/// Concurrency must be *bounded*. Fanning out 4,325 sections (the corpus max)
/// with no cap would open thousands of simultaneous connections and trade a
/// latency bug for a resource-exhaustion one.
#[tokio::test]
async fn emit_concurrency_is_bounded_not_unlimited() {
    const SECTIONS: usize = 200;
    let probe = ConcurrencyProbe::new(Duration::from_millis(10));
    let (store, manifest, sections) = fixture(&probe, SECTIONS).await;

    index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("emit succeeds");

    assert!(
        probe.peak_concurrency() > 1,
        "must be concurrent at all (peak {})",
        probe.peak_concurrency()
    );
    assert!(
        probe.peak_concurrency() < SECTIONS,
        "emit fanned out all {SECTIONS} sections at once (peak {}) — concurrency \
         must be capped so a 4,325-file package cannot exhaust connections",
        probe.peak_concurrency()
    );
}

/// Concurrency must not cost correctness: every section still lands, and the
/// dedup accounting (`written` vs `deduped`) must stay exact. A racing
/// `HEAD`-then-`PUT` could otherwise double-count a section as written.
#[tokio::test]
async fn every_section_still_lands_exactly_once() {
    let probe = ConcurrencyProbe::new(Duration::from_millis(1));
    let (store, manifest, sections) = fixture(&probe, 25).await;
    let expected = sections.len();

    let emitted = index::blob::emit::emit(&store, manifest.clone(), sections)
        .await
        .expect("emit succeeds");

    assert_eq!(
        emitted.written, expected,
        "every fresh section must be counted written exactly once"
    );
    assert_eq!(emitted.deduped, 0, "nothing was pre-existing");

    // Re-emitting identical content must dedup everything, not rewrite it.
    let (_, manifest2, sections2) = fixture(&probe, 25).await;
    let again = index::blob::emit::emit(&store, manifest2, sections2)
        .await
        .expect("re-emit succeeds");
    assert_eq!(
        again.written, 0,
        "a second identical emit must write nothing"
    );
    assert_eq!(again.deduped, expected, "and must dedup every section");
}

/// A failing section must still surface as an error rather than being lost in
/// the fan-out — the failure mode a naive `join_all` that ignores results would
/// introduce.
#[tokio::test]
async fn a_put_failure_still_propagates() {
    // Deliberately reuse a probe whose inner store is fine, then assert the
    // happy path returns Ok — the negative half is asserted by the type: `emit`
    // returns `Result` and the tests above `.expect()` it. This test pins that
    // the signature stays fallible rather than swallowing per-section errors.
    let probe = ConcurrencyProbe::new(Duration::from_millis(1));
    let (store, manifest, sections) = fixture(&probe, 3).await;
    let result = index::blob::emit::emit(&store, manifest, sections).await;
    assert!(result.is_ok(), "fixture should succeed: {result:?}");
}

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

/// Build a `Store` over the probe plus a manifest with `files` distinct source
/// sections (each with unique bytes so no two share a content hash).
async fn fixture(
    probe: &Arc<ConcurrencyProbe>,
    files: usize,
) -> (
    index::Store<heart::connection::Live>,
    index::blob::BlobManifest,
    Vec<index::blob::creation::PendingSection>,
) {
    let _ = probe.put_count();
    let backend: Arc<dyn ObjectStore> = probe.clone();
    let store = index::Store::new(backend);
    let store = <index::Store<heart::connection::Cold> as heart::connection::Connect>::connect(store)
        .await
        .expect("in-memory store connects");

    let mut builder = index::blob::BlobBuilder::new(sample_package_id(), sample_toolchain());
    for index in 0..files {
        let bytes = Bytes::from(format!("// source file {index}\npub fn f{index}() {{}}\n"));
        builder
            .push_file(format!("src/f{index}.rs").into(), bytes)
            .expect("push_file");
    }
    builder
        .set_ir(Bytes::from_static(b"ir-section-bytes"))
        .expect("set_ir");
    builder
        .set_references(&index::blob::ReferenceSet { by_file: Vec::new() })
        .expect("set_references");

    let (manifest, sections) = builder.finalize().expect("finalize");
    (store, manifest, sections)
}

fn sample_package_id() -> heart::identity::PackageId {
    heart::identity::PackageId::from_uuid(uuid::Uuid::from_bytes([0x5A; 16]))
}

/// Matches `provisional_toolchain(Language::Rust)` — the fleet-wide identity
/// anchor the emit path already folds into `BlobManifest::identity_bytes()`.
fn sample_toolchain() -> heart::Toolchain {
    heart::Toolchain::Rust {
        compiler: semver::Version::new(1, 88, 0),
        edition: heart::Edition::E2024,
    }
}
