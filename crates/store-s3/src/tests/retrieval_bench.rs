//! Reproducible retrieval measurements for FileStore and the production S3
//! pack route over the crate's real loopback HTTP server.
//!
//! Run explicitly with:
//!
//! ```text
//! cargo test --locked --offline -p backend-store-s3 --lib retrieval_bench -- --ignored --nocapture
//! ```
//!
//! This is a measurement harness, not a correctness-only mock: uploads and
//! range GETs traverse `S3PackRoute` and a TCP loopback S3-compatible server.
//! The historical S3 lane retains its serial, close-after-response fixture; a
//! separately labeled concurrency lane uses a bounded worker pool and shares
//! immutable bytes with a precomputed checksum. Both remain in-memory loopback
//! measurements and do not predict a remote S3 service. FileStore cold samples
//! run in a fresh test process, but the OS page cache is not flushed. The S3
//! API admits only a complete verified extent before exposing its temporary
//! file, so its first-byte metric is consumer-visible latency, not network TTFB.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::unwrap_used
)]

#[path = "retrieval_bench_server.rs"]
mod concurrent_loopback;

use std::{
    collections::HashMap,
    env, fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{CheckedPackedObject, ImmutableS3Pack, S3RemotePack};
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactObjectClaim, ArtifactObjectReader, ArtifactPlan,
    ClosureId, ClosureManifest, FileStore, StoreError, TypedObject, UntrustedObjectId,
};

const SAMPLE_ENV: &str = "BACKEND_STORE_RETRIEVAL_SAMPLES";
const COLD_WORKER_ENV: &str = "BACKEND_STORE_RETRIEVAL_COLD_WORKER";
const COLD_ROOT_ENV: &str = "BACKEND_STORE_RETRIEVAL_COLD_ROOT";
const COLD_INDEX_ENV: &str = "BACKEND_STORE_RETRIEVAL_COLD_INDEX";
const COLD_SIZE_ENV: &str = "BACKEND_STORE_RETRIEVAL_COLD_SIZE";
const COLD_QUERY_KIND_ENV: &str = "BACKEND_STORE_RETRIEVAL_COLD_QUERY_KIND";
const COLD_QUERY_INDEX_ENV: &str = "BACKEND_STORE_RETRIEVAL_COLD_QUERY_INDEX";
const CHUNK_BYTES: usize = 64 * 1024;
const PACK_LIMIT_BYTES: u64 = 32 * 1024 * 1024;
const OBJECT_HEADER_BYTES: usize = b"LUNA_OBJECT_V1\0".len() + 32 + 1 + 2 + 1 + 32 + 32 + 8;
const LARGE_OBJECT_SIZES: [usize; 4] = [1024, 64 * 1024, 1024 * 1024, 16 * 1024 * 1024];
const DOCUMENT_COUNT: usize = 10;
const RECORDS_PER_DOCUMENT: usize = 100;
const DOCUMENT_OBJECT_COUNT: usize = DOCUMENT_COUNT * RECORDS_PER_DOCUMENT;
const DOCUMENT_PAYLOAD_BYTES: usize = 1024;
const RANGE_RECORD_COUNT: usize = 8;
const RANGE_BYTES: usize = 32;
const CONCURRENT_LOOPBACK_WORKERS: usize = 16;
const BENCHMARK_SEED: u64 = 0x5eed_cafe_d00d_beef;
static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct BenchDirectory(PathBuf);

impl BenchDirectory {
    fn new(label: &str) -> std::io::Result<Self> {
        let ordinal = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "backend-store-retrieval-{label}-{}-{nonce}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for BenchDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy, Debug)]
struct Timing {
    first_byte_ns: u128,
    complete_ns: u128,
}

/// Adapts the store's intentionally non-`Error` enum for measurement helpers
/// that combine store, I/O, and process failures behind `dyn Error`.
#[derive(Debug)]
struct StoreBenchError(StoreError);

impl std::fmt::Display for StoreBenchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "backend-store: {:?}", self.0)
    }
}

impl std::error::Error for StoreBenchError {}

fn store_result<T>(result: Result<T, StoreError>) -> Result<T, StoreBenchError> {
    result.map_err(StoreBenchError)
}

#[derive(Default)]
struct Rng(u64);

impl Rng {
    fn seeded(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        // Fixed xorshift64 sequence: fast, repeatable selection for fixture
        // addressing; it is not used for cryptographic material.
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    fn below(&mut self, upper: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(upper).expect("positive bound"))
            .expect("bounded random index")
    }
}

fn samples() -> usize {
    env::var(SAMPLE_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (5..=501).contains(value))
        .unwrap_or(21)
}

// The expected-byte oracle is deliberately separate from `TypedObject`'s
// backing byte slice. Values vary with the byte offset, so an off-by-one seek
// or a range that silently returns the beginning of an object cannot pass
// merely because the fixture payload is repeated.
fn oracle_payload_byte(offset: usize) -> u8 {
    let mixed = offset
        .wrapping_mul(37)
        .wrapping_add((offset >> 8).wrapping_mul(17))
        .wrapping_add((offset >> 16).wrapping_mul(101))
        .wrapping_add(13);
    u8::try_from(mixed % 251).expect("modulo 251 fits in u8")
}

fn benchmark_object(index: usize, payload_bytes: usize) -> TypedObject {
    let key_bytes = format!("retrieval-bench-key-{index:06}").into_bytes();
    let key =
        backend_version::ObjectKey::<super::TestBytesSchema>::from_value(key_bytes.as_slice());
    let payload = (0..payload_bytes)
        .map(oracle_payload_byte)
        .collect::<Vec<_>>();
    TypedObject::from_value(&key, payload.as_slice())
}

fn matches_oracle(actual: &[u8], offset: usize) -> bool {
    actual
        .iter()
        .enumerate()
        .all(|(index, byte)| *byte == oracle_payload_byte(offset + index))
}

fn local_budget(objects: &[TypedObject]) -> ArtifactBudget {
    let payload_bytes = objects
        .iter()
        .map(|object| u64::try_from(object.bytes().len()).expect("fixture size fits u64"))
        .sum::<u64>();
    let put_calls = objects
        .iter()
        .map(|object| object.bytes().len().div_ceil(CHUNK_BYTES))
        .sum::<usize>()
        .saturating_add(1);
    ArtifactBudget::new(
        objects.len().max(1),
        objects.len().max(1),
        payload_bytes.max(1),
        CHUNK_BYTES,
        put_calls,
    )
}

fn object_claim(object: &TypedObject) -> ArtifactObjectClaim {
    ArtifactObjectClaim::new(
        object.schema(),
        *object.key(),
        *object.version(),
        u64::try_from(object.bytes().len()).expect("fixture size fits u64"),
    )
    .with_object_id(UntrustedObjectId::from_bytes(*object.id().as_bytes()))
}

fn persist_local_objects(
    root: &Path,
    objects: &[TypedObject],
) -> Result<(FileStore, ClosureId, ArtifactBudget, u64), Box<dyn std::error::Error>> {
    let budget = local_budget(objects);
    let store = store_result(FileStore::open(root, usize::try_from(PACK_LIMIT_BYTES)?))?;
    let closure = store_result(ClosureManifest::new(objects.to_vec()))?.id();
    let claims = objects.iter().map(object_claim).collect::<Vec<_>>();
    let mut session = store_result(store.artifact_sink(budget).begin(ArtifactPlan::new(
        None,
        ArtifactClosureClaim::from_id(closure),
        claims,
        Vec::new(),
    )))?;
    for (object_index, object) in objects.iter().enumerate() {
        for (chunk_index, chunk) in object.bytes().chunks(CHUNK_BYTES).enumerate() {
            let offset = u64::try_from(
                chunk_index
                    .checked_mul(CHUNK_BYTES)
                    .ok_or("offset overflow")?,
            )?;
            store_result(session.put(object_index, offset, chunk))?;
        }
    }
    let receipt = store_result(session.finish())?;
    assert_eq!(receipt.closure(), closure);
    assert_eq!(
        receipt.object_count(),
        u64::try_from(objects.len()).expect("fixture object count fits u64")
    );
    let bytes_written = receipt.bytes_written();
    Ok((store, closure, budget, bytes_written))
}

fn build_pack(objects: &[TypedObject]) -> Result<ImmutableS3Pack, super::RemoteStoreError> {
    let mut builder = super::S3PackBuilder::new(PACK_LIMIT_BYTES)?;
    for object in objects {
        builder.push(object)?;
    }
    builder.finish()
}

fn coalesced_route(
    server: &super::LoopbackServer,
    maximum_bytes: u64,
) -> Result<super::S3PackRoute, Box<dyn std::error::Error>> {
    Ok(
        super::S3PackRoute::new(server.route_with_max(maximum_bytes))
            .with_coalesced_directory_prefetch(crate::MAX_COALESCED_DIRECTORY_PREFIX_BYTES)?,
    )
}

fn coalesced_prefix_bytes(pack: &ImmutableS3Pack) -> Result<u64, Box<dyn std::error::Error>> {
    let upper_bound = backend_store::artifact_pack::artifact_pack_directory_prefix_upper_bound(
        pack.manifest_bytes(),
    )
    .ok_or("pack root has no conservative directory-prefix bound")?;
    if upper_bound > crate::MAX_COALESCED_DIRECTORY_PREFIX_BYTES {
        return Err("pack directory exceeds coalesced-read policy cap".into());
    }
    let upper_bound = u64::try_from(upper_bound)?;
    Ok(if upper_bound < pack.len() {
        upper_bound
    } else {
        u64::from(pack.manifest_bytes())
    })
}

fn object_id(object: &TypedObject) -> UntrustedObjectId {
    UntrustedObjectId::from_bytes(*object.id().as_bytes())
}

fn read_local_range(
    reader: &mut ArtifactObjectReader,
    object: &TypedObject,
    offset: usize,
    length: usize,
    started: Instant,
) -> Result<Timing, Box<dyn std::error::Error>> {
    assert_eq!(reader.id(), object.id());
    let end = offset.checked_add(length).ok_or("range overflow")?;
    assert!(end <= object.bytes().len());
    let mut first_byte_ns = None;
    let mut consumed = 0usize;
    let mut buffer = [0_u8; CHUNK_BYTES];
    while consumed < length {
        let count = (length - consumed).min(buffer.len());
        let read = store_result(reader.read_payload_range(
            u64::try_from(offset.checked_add(consumed).ok_or("range overflow")?)?,
            &mut buffer[..count],
        ))?;
        if read == 0 {
            return Err("local payload range ended early".into());
        }
        if first_byte_ns.is_none() {
            first_byte_ns = Some(started.elapsed().as_nanos());
        }
        let expected_start = offset.checked_add(consumed).ok_or("range overflow")?;
        assert!(matches_oracle(&buffer[..read], expected_start));
        consumed = consumed.checked_add(read).ok_or("read count overflow")?;
    }
    Ok(Timing {
        first_byte_ns: first_byte_ns.unwrap_or_else(|| started.elapsed().as_nanos()),
        complete_ns: started.elapsed().as_nanos(),
    })
}

fn read_remote_range(
    object: &CheckedPackedObject,
    expected: &TypedObject,
    offset: usize,
    length: usize,
    started: Instant,
) -> Result<Timing, Box<dyn std::error::Error>> {
    assert_eq!(object.id(), expected.id());
    let end = offset.checked_add(length).ok_or("range overflow")?;
    assert!(end <= expected.bytes().len());
    let mut envelope = object.open_envelope()?;
    envelope.seek(SeekFrom::Start(u64::try_from(
        OBJECT_HEADER_BYTES
            .checked_add(offset)
            .ok_or("offset overflow")?,
    )?))?;
    let mut first_byte_ns = None;
    let mut consumed = 0usize;
    let mut buffer = [0_u8; CHUNK_BYTES];
    while consumed < length {
        let count = (length - consumed).min(buffer.len());
        envelope.read_exact(&mut buffer[..count])?;
        if first_byte_ns.is_none() {
            first_byte_ns = Some(started.elapsed().as_nanos());
        }
        let expected_start = offset.checked_add(consumed).ok_or("range overflow")?;
        assert!(matches_oracle(&buffer[..count], expected_start));
        consumed = consumed.checked_add(count).ok_or("read count overflow")?;
    }
    Ok(Timing {
        first_byte_ns: first_byte_ns.unwrap_or_else(|| started.elapsed().as_nanos()),
        complete_ns: started.elapsed().as_nanos(),
    })
}

fn percentile(values: impl Iterator<Item = u128>, count: usize, percentile: usize) -> u128 {
    let mut values = values.collect::<Vec<_>>();
    values.sort_unstable();
    let rank = count
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1)
        .min(values.len().saturating_sub(1));
    values.get(rank).copied().unwrap_or_default()
}

fn report_timings(label: &str, timings: &[Timing], bytes_read: u64, bytes_returned: u64) {
    let count = timings.len();
    let first = |p| percentile(timings.iter().map(|sample| sample.first_byte_ns), count, p);
    let total = |p| percentile(timings.iter().map(|sample| sample.complete_ns), count, p);
    println!(
        "retrieval_bench lane={label} samples={count} first_byte_ns_p50={} first_byte_ns_p95={} first_byte_ns_p99={} complete_ns_p50={} complete_ns_p95={} complete_ns_p99={} read_bytes_per_sample={bytes_read} result_bytes_per_sample={bytes_returned} amplification_x={:.3}",
        first(50),
        first(95),
        first(99),
        total(50),
        total(95),
        total(99),
        if bytes_returned == 0 {
            0.0
        } else {
            bytes_read as f64 / bytes_returned as f64
        },
    );
}

fn parse_worker_line(output: &std::process::Output) -> Result<Timing, Box<dyn std::error::Error>> {
    if !output.status.success() {
        return Err(format!(
            "cold FileStore worker failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("BENCH_LOCAL_COLD_NS "))
        .ok_or("cold FileStore worker did not report timing")?;
    let mut fields = line.split_whitespace();
    assert_eq!(fields.next(), Some("BENCH_LOCAL_COLD_NS"));
    let first_byte_ns = fields.next().ok_or("missing first-byte timing")?.parse()?;
    let complete_ns = fields.next().ok_or("missing complete timing")?.parse()?;
    Ok(Timing {
        first_byte_ns,
        complete_ns,
    })
}

fn benchmark_test_filter() -> Result<String, Box<dyn std::error::Error>> {
    let listed = Command::new(std::env::current_exe()?)
        .arg("--list")
        .output()?;
    if !listed.status.success() {
        return Err("could not list unit test names for cold process samples".into());
    }
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .find(|name| {
            name.ends_with("::retrieval_benchmark_reports_latency_bytes_and_amplification")
        })
        .map(str::to_owned)
        .ok_or_else(|| "retrieval benchmark test filter was not listed".into())
}

fn local_cold_process_samples(
    root: &Path,
    object_index: usize,
    payload_len: usize,
    sample_count: usize,
    test_filter: &str,
) -> Result<Vec<Timing>, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let mut measurements = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let output = Command::new(&executable)
            .arg("--exact")
            .arg(test_filter)
            .arg("--ignored")
            .arg("--nocapture")
            .env(COLD_WORKER_ENV, "1")
            .env(COLD_ROOT_ENV, root)
            .env(COLD_INDEX_ENV, object_index.to_string())
            .env(COLD_SIZE_ENV, payload_len.to_string())
            .output()?;
        measurements.push(parse_worker_line(&output)?);
    }
    Ok(measurements)
}

fn local_storage_bytes(root: &Path) -> std::io::Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            total = total
                .checked_add(local_storage_bytes(&entry.path())?)
                .ok_or_else(|| std::io::Error::other("storage byte count overflow"))?;
        } else if kind.is_file() {
            total = total
                .checked_add(entry.metadata()?.len())
                .ok_or_else(|| std::io::Error::other("storage byte count overflow"))?;
        }
    }
    Ok(total)
}

fn verify_stored_pack(server: super::LoopbackServer, pack: &ImmutableS3Pack) {
    let saved = server.finish();
    let saved = saved.lock().expect("loopback object mutex");
    let stored = saved
        .as_ref()
        .expect("loopback received one immutable pack");
    let mut expected = pack.open_pack().expect("open deterministic source pack");
    let mut buffer = [0_u8; CHUNK_BYTES];
    let mut offset = 0usize;
    loop {
        let count = expected.read(&mut buffer).expect("read source pack");
        if count == 0 {
            break;
        }
        assert_eq!(&stored[offset..offset + count], &buffer[..count]);
        offset = offset.checked_add(count).expect("pack offset");
    }
    assert_eq!(offset, stored.len());
}

fn benchmark_one_size(
    payload_len: usize,
    sample_count: usize,
    test_filter: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let object = benchmark_object(17, payload_len);
    let objects = vec![object.clone()];
    let directory = BenchDirectory::new(&format!("size-{payload_len}"))?;
    let (store, closure, budget, local_bytes_written) =
        persist_local_objects(directory.path(), &objects)?;
    drop(store);
    let local_storage = local_storage_bytes(directory.path())?;
    let payload_bytes = u64::try_from(payload_len)?;
    let envelope_bytes = u64::try_from(
        OBJECT_HEADER_BYTES
            .checked_add(payload_len)
            .ok_or("envelope overflow")?,
    )?;

    let cold_local =
        local_cold_process_samples(directory.path(), 17, payload_len, sample_count, test_filter)?;
    report_timings(
        &format!("filestore.cold_process_reopen.size={payload_len}"),
        &cold_local,
        envelope_bytes + payload_bytes,
        payload_bytes,
    );

    let store = store_result(FileStore::open(
        directory.path(),
        usize::try_from(PACK_LIMIT_BYTES)?,
    ))?;
    let sink = store.artifact_sink(budget);
    let mut warm_reader = sink
        .open_object(object_id(&object))
        .map_err(StoreBenchError)?
        .ok_or("warm local object disappeared")?;
    let mut warm_local = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let started = Instant::now();
        warm_local.push(read_local_range(
            &mut warm_reader,
            &object,
            0,
            payload_len,
            started,
        )?);
    }
    report_timings(
        &format!("filestore.warm_reader.size={payload_len}"),
        &warm_local,
        payload_bytes,
        payload_bytes,
    );

    let pack = build_pack(&objects)?;
    let prefetched_prefix_bytes = coalesced_prefix_bytes(&pack)?;
    let first_page_bytes = u64::from(
        pack.manifest()
            .pages()
            .first()
            .ok_or("single-object pack has no directory page")?
            .bytes(),
    );
    let coalesced = prefetched_prefix_bytes > u64::from(pack.manifest_bytes());
    let cold_directory_bytes =
        prefetched_prefix_bytes + if coalesced { 0 } else { first_page_bytes };
    let request_count = 2usize
        .checked_add(
            sample_count
                .checked_mul(if coalesced { 3 } else { 4 })
                .ok_or("request count overflow")?,
        )
        .and_then(|count| count.checked_add(if coalesced { 0 } else { 1 }))
        .ok_or("request count overflow")?;
    let server = super::LoopbackServer::start(super::ServerMode::Store, request_count, None);
    let route = coalesced_route(&server, PACK_LIMIT_BYTES)?;
    let upload = super::pack_upload_capability(&server, &pack, super::fence());
    let receipt = route.put_pack(&pack, &upload, super::fence(), None)?;
    assert_eq!(receipt.pack_id(), pack.pack_id());
    assert_eq!(receipt.layout_id(), pack.layout_id());
    assert_eq!(receipt.object_count(), 1);
    let read = super::pack_read_capability(&server, &pack, super::fence(), true);

    let mut cold_s3 = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let started = Instant::now();
        let cold_route = coalesced_route(&server, PACK_LIMIT_BYTES)?;
        let cold_pack = cold_route.open_pack(&read, super::fence())?;
        let checked = cold_route.get_object(
            &cold_pack,
            object.id(),
            super::fence(),
            &backend_store::RelationAdmissionRegistry::default(),
        )?;
        assert_eq!(checked.pack_id(), pack.pack_id());
        assert_eq!(checked.layout_id(), pack.layout_id());
        cold_s3.push(read_remote_range(
            &checked,
            &object,
            0,
            payload_len,
            started,
        )?);
    }
    report_timings(
        &format!("s3_loopback.cold_route_reopen.size={payload_len}"),
        &cold_s3,
        cold_directory_bytes + envelope_bytes,
        payload_bytes,
    );

    let warm_route = coalesced_route(&server, PACK_LIMIT_BYTES)?;
    let warm_pack = warm_route.open_pack(&read, super::fence())?;
    let mut warm_s3 = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let started = Instant::now();
        let checked = warm_route.get_object(
            &warm_pack,
            object.id(),
            super::fence(),
            &backend_store::RelationAdmissionRegistry::default(),
        )?;
        warm_s3.push(read_remote_range(
            &checked,
            &object,
            0,
            payload_len,
            started,
        )?);
    }
    report_timings(
        &format!("s3_loopback.warm_pack.size={payload_len}"),
        &warm_s3,
        if coalesced {
            envelope_bytes
        } else {
            first_page_bytes / u64::try_from(sample_count.max(1))? + envelope_bytes
        },
        payload_bytes,
    );

    let stats = server.stats();
    assert_eq!(stats.requests, request_count);
    assert_eq!(stats.put_requests, 1);
    assert_eq!(stats.get_requests, request_count - 1);
    assert_eq!(u64::try_from(stats.request_body_bytes)?, pack.len());
    let expected_response_bytes = prefetched_prefix_bytes
        .checked_mul(u64::try_from(sample_count + 1)?)
        .and_then(|value| {
            value.checked_add(if coalesced {
                0
            } else {
                first_page_bytes.checked_mul(u64::try_from(sample_count + 1).ok()?)?
            })
        })
        .and_then(|value| {
            value.checked_add(envelope_bytes.checked_mul(u64::try_from(sample_count * 2).ok()?)?)
        })
        .ok_or("response byte count overflow")?;
    assert_eq!(
        u64::try_from(stats.response_body_bytes)?,
        expected_response_bytes
    );
    verify_stored_pack(server, &pack);

    println!(
        "retrieval_bench fixture=size payload_bytes={payload_bytes} envelope_bytes={envelope_bytes} pack_bytes={} pack_objects=1 closure={} local_bytes_written={local_bytes_written} local_store_file_bytes={local_storage} local_amplification_x={:.3} s3_storage_amplification_x={:.3} upload_bytes={} response_bytes={} retries=0 seed={BENCHMARK_SEED}",
        pack.len(),
        hex(closure.as_bytes()),
        local_storage as f64 / payload_bytes as f64,
        pack.len() as f64 / payload_bytes as f64,
        stats.request_body_bytes,
        stats.response_body_bytes,
    );
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct QueryEntry {
    object_index: usize,
    offset: usize,
}

#[derive(Clone, Debug)]
struct Query {
    entries: Vec<QueryEntry>,
}

fn clustered_document_fixture() -> (Vec<TypedObject>, Vec<Vec<[u8; 32]>>) {
    let mut objects = Vec::with_capacity(DOCUMENT_OBJECT_COUNT);
    let mut documents = Vec::with_capacity(DOCUMENT_COUNT);
    for document in 0..DOCUMENT_COUNT {
        let mut members = Vec::with_capacity(RECORDS_PER_DOCUMENT);
        for record in 0..RECORDS_PER_DOCUMENT {
            let object_index = document
                .checked_mul(RECORDS_PER_DOCUMENT)
                .and_then(|value| value.checked_add(record))
                .expect("bounded document fixture index");
            let object = benchmark_object(object_index, DOCUMENT_PAYLOAD_BYTES);
            members.push(*object.id().as_bytes());
            objects.push(object);
        }
        documents.push(members);
    }
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    (objects, documents)
}

fn make_queries(
    documents: &[Vec<[u8; 32]>],
    object_positions: &HashMap<[u8; 32], usize>,
    sample_count: usize,
) -> (Vec<Query>, Vec<Query>) {
    let mut rng = Rng::seeded(BENCHMARK_SEED ^ 0x1000_64);
    let mut points = Vec::with_capacity(sample_count);
    let mut ranges = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let point_document = rng.below(documents.len());
        let point_record = rng.below(RECORDS_PER_DOCUMENT);
        let point_id = documents[point_document][point_record];
        points.push(Query {
            entries: vec![QueryEntry {
                object_index: *object_positions
                    .get(&point_id)
                    .expect("point fixture ID has a sorted object"),
                offset: rng.below(DOCUMENT_PAYLOAD_BYTES - RANGE_BYTES),
            }],
        });

        let document = rng.below(documents.len());
        let first_record = rng.below(RECORDS_PER_DOCUMENT - RANGE_RECORD_COUNT + 1);
        let entries = documents[document][first_record..first_record + RANGE_RECORD_COUNT]
            .iter()
            .enumerate()
            .map(|(range_member, id)| QueryEntry {
                object_index: *object_positions
                    .get(id)
                    .expect("range fixture ID has a sorted object"),
                // Different offsets within one logical document range make
                // extent/start confusion visible. The first two exercise both
                // payload boundaries; remaining members use sparse random
                // offsets.
                offset: match range_member % 3 {
                    0 => 0,
                    1 => DOCUMENT_PAYLOAD_BYTES - RANGE_BYTES,
                    _ => rng.below(DOCUMENT_PAYLOAD_BYTES - RANGE_BYTES),
                },
            })
            .collect();
        ranges.push(Query { entries });
    }
    (points, ranges)
}

fn run_local_query(
    sink: &backend_store::ArtifactSink,
    objects: &[TypedObject],
    query: &Query,
    started: Instant,
) -> Result<Timing, Box<dyn std::error::Error>> {
    let mut first_byte_ns = None;
    for entry in &query.entries {
        let object = objects
            .get(entry.object_index)
            .ok_or("query object index outside fixture")?;
        let mut reader = sink
            .open_object(object_id(object))
            .map_err(StoreBenchError)?
            .ok_or("local query object disappeared")?;
        let timing = read_local_range(&mut reader, object, entry.offset, RANGE_BYTES, started)?;
        first_byte_ns.get_or_insert(timing.first_byte_ns);
    }
    Ok(Timing {
        first_byte_ns: first_byte_ns.unwrap_or_else(|| started.elapsed().as_nanos()),
        complete_ns: started.elapsed().as_nanos(),
    })
}

fn run_local_warm_query(
    readers: &mut [ArtifactObjectReader],
    objects: &[TypedObject],
    query: &Query,
) -> Result<Timing, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut first_byte_ns = None;
    for entry in &query.entries {
        let object = objects
            .get(entry.object_index)
            .ok_or("query object index outside fixture")?;
        let reader = readers
            .get_mut(entry.object_index)
            .ok_or("warm reader index outside fixture")?;
        let timing = read_local_range(reader, object, entry.offset, RANGE_BYTES, started)?;
        first_byte_ns.get_or_insert(timing.first_byte_ns);
    }
    Ok(Timing {
        first_byte_ns: first_byte_ns.unwrap_or_else(|| started.elapsed().as_nanos()),
        complete_ns: started.elapsed().as_nanos(),
    })
}

fn run_s3_query(
    route: &super::S3PackRoute,
    pack: &S3RemotePack,
    objects: &[TypedObject],
    query: &Query,
    started: Instant,
) -> Result<Timing, Box<dyn std::error::Error>> {
    let mut first_byte_ns = None;
    let mut requested = Vec::with_capacity(query.entries.len());
    for entry in &query.entries {
        let object = objects
            .get(entry.object_index)
            .ok_or("query object index outside fixture")?;
        requested.push(object.id());
    }
    let checked_objects = route.get_objects(
        pack,
        &requested,
        super::fence(),
        &backend_store::RelationAdmissionRegistry::default(),
    )?;
    if !checked_objects.is_empty() {
        // `get_objects` returns only after every requested envelope is fully
        // verified and available; this is the earliest consumer-visible byte.
        first_byte_ns = Some(started.elapsed().as_nanos());
    }
    for (entry, checked) in query.entries.iter().zip(checked_objects) {
        let object = objects
            .get(entry.object_index)
            .ok_or("query object index outside fixture")?;
        let timing = read_remote_range(&checked, object, entry.offset, RANGE_BYTES, started)?;
        first_byte_ns.get_or_insert(timing.first_byte_ns);
    }
    Ok(Timing {
        first_byte_ns: first_byte_ns.unwrap_or_else(|| started.elapsed().as_nanos()),
        complete_ns: started.elapsed().as_nanos(),
    })
}

fn local_cold_query_process_samples(
    root: &Path,
    query_kind: &str,
    sample_count: usize,
    test_filter: &str,
) -> Result<Vec<Timing>, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let mut measurements = Vec::with_capacity(sample_count);
    for query_index in 0..sample_count {
        let output = Command::new(&executable)
            .arg("--exact")
            .arg(test_filter)
            .arg("--ignored")
            .arg("--nocapture")
            .env(COLD_WORKER_ENV, "1")
            .env(COLD_ROOT_ENV, root)
            .env(COLD_QUERY_KIND_ENV, query_kind)
            .env(COLD_QUERY_INDEX_ENV, query_index.to_string())
            .output()?;
        measurements.push(parse_worker_line(&output)?);
    }
    Ok(measurements)
}

fn benchmark_clustered_documents(
    sample_count: usize,
    test_filter: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (objects, documents) = clustered_document_fixture();
    let mut positions = HashMap::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        positions.insert(*object.id().as_bytes(), index);
    }
    let (point_queries, range_queries) = make_queries(&documents, &positions, sample_count);
    let directory = BenchDirectory::new("clustered-documents")?;
    let (store, closure, budget, local_bytes_written) =
        persist_local_objects(directory.path(), &objects)?;
    let local_storage = local_storage_bytes(directory.path())?;
    let payload_bytes = u64::try_from(DOCUMENT_OBJECT_COUNT * DOCUMENT_PAYLOAD_BYTES)?;
    let envelope_bytes = objects
        .iter()
        .map(|object| u64::try_from(OBJECT_HEADER_BYTES + object.bytes().len()).expect("envelope"))
        .sum::<u64>();

    let cold_point =
        local_cold_query_process_samples(directory.path(), "point", sample_count, test_filter)?;
    let cold_range =
        local_cold_query_process_samples(directory.path(), "range", sample_count, test_filter)?;

    let warm_sink = store.artifact_sink(budget);
    let mut warm_readers = Vec::with_capacity(objects.len());
    for object in &objects {
        warm_readers.push(
            warm_sink
                .open_object(object_id(object))
                .map_err(StoreBenchError)?
                .ok_or("warm clustered document object disappeared")?,
        );
    }
    let mut warm_point = Vec::with_capacity(sample_count);
    let mut warm_range = Vec::with_capacity(sample_count);
    for query in &point_queries {
        warm_point.push(run_local_warm_query(&mut warm_readers, &objects, query)?);
    }
    for query in &range_queries {
        warm_range.push(run_local_warm_query(&mut warm_readers, &objects, query)?);
    }

    let pack = build_pack(&objects)?;
    let prefetched_prefix_bytes = coalesced_prefix_bytes(&pack)?;
    let request_count = 2usize
        .checked_add(
            sample_count
                .checked_mul(20)
                .ok_or("request count overflow")?,
        )
        .ok_or("request count overflow")?;
    let server = super::LoopbackServer::start(super::ServerMode::Store, request_count, None);
    let route = coalesced_route(&server, PACK_LIMIT_BYTES)?;
    let upload = super::pack_upload_capability(&server, &pack, super::fence());
    let receipt = route.put_pack(&pack, &upload, super::fence(), None)?;
    assert_eq!(
        receipt.object_count(),
        u32::try_from(DOCUMENT_OBJECT_COUNT).expect("fixed fixture count fits u32")
    );
    assert_eq!(receipt.pack_id(), pack.pack_id());
    let read = super::pack_read_capability(&server, &pack, super::fence(), true);

    let mut cold_s3_point = Vec::with_capacity(sample_count);
    let mut cold_s3_range = Vec::with_capacity(sample_count);
    for query in &point_queries {
        let cold_route = coalesced_route(&server, PACK_LIMIT_BYTES)?;
        let started = Instant::now();
        let cold_pack = cold_route.open_pack(&read, super::fence())?;
        cold_s3_point.push(run_s3_query(
            &cold_route,
            &cold_pack,
            &objects,
            query,
            started,
        )?);
    }
    for query in &range_queries {
        let cold_route = coalesced_route(&server, PACK_LIMIT_BYTES)?;
        let started = Instant::now();
        let cold_pack = cold_route.open_pack(&read, super::fence())?;
        cold_s3_range.push(run_s3_query(
            &cold_route,
            &cold_pack,
            &objects,
            query,
            started,
        )?);
    }

    let warm_route = coalesced_route(&server, PACK_LIMIT_BYTES)?;
    let warm_pack = warm_route.open_pack(&read, super::fence())?;
    let mut warm_s3_point = Vec::with_capacity(sample_count);
    let mut warm_s3_range = Vec::with_capacity(sample_count);
    for query in &point_queries {
        warm_s3_point.push(run_s3_query(
            &warm_route,
            &warm_pack,
            &objects,
            query,
            Instant::now(),
        )?);
    }
    for query in &range_queries {
        warm_s3_range.push(run_s3_query(
            &warm_route,
            &warm_pack,
            &objects,
            query,
            Instant::now(),
        )?);
    }

    let point_local_bytes =
        u64::try_from(OBJECT_HEADER_BYTES + DOCUMENT_PAYLOAD_BYTES + RANGE_BYTES)?;
    let point_returned = u64::try_from(RANGE_BYTES)?;
    let range_local_bytes = point_local_bytes
        .checked_mul(u64::try_from(RANGE_RECORD_COUNT)?)
        .ok_or("local range byte count overflow")?;
    let range_returned = point_returned
        .checked_mul(u64::try_from(RANGE_RECORD_COUNT)?)
        .ok_or("range result byte count overflow")?;
    report_timings(
        "documents.local.cold_verify.point",
        &cold_point,
        point_local_bytes,
        point_returned,
    );
    report_timings(
        "documents.local.warm_reader.point",
        &warm_point,
        point_returned,
        point_returned,
    );
    report_timings(
        "documents.local.cold_verify.doc_range_8",
        &cold_range,
        range_local_bytes,
        range_returned,
    );
    report_timings(
        "documents.local.warm_reader.doc_range_8",
        &warm_range,
        range_returned,
        range_returned,
    );
    let point_envelope = u64::try_from(OBJECT_HEADER_BYTES + DOCUMENT_PAYLOAD_BYTES)?;
    let range_s3_bytes = point_envelope
        .checked_mul(u64::try_from(RANGE_RECORD_COUNT)?)
        .ok_or("S3 range byte count overflow")?;
    report_timings(
        "documents.s3_loopback.cold_route_reopen.point",
        &cold_s3_point,
        prefetched_prefix_bytes + point_envelope,
        point_returned,
    );
    report_timings(
        "documents.s3_loopback.warm_pack.point",
        &warm_s3_point,
        point_envelope,
        point_returned,
    );
    report_timings(
        "documents.s3_loopback.cold_route_reopen.doc_range_8",
        &cold_s3_range,
        prefetched_prefix_bytes + range_s3_bytes,
        range_returned,
    );
    report_timings(
        "documents.s3_loopback.warm_pack.doc_range_8",
        &warm_s3_range,
        range_s3_bytes,
        range_returned,
    );

    let stats = server.stats();
    assert_eq!(stats.requests, request_count);
    assert_eq!(stats.put_requests, 1);
    assert_eq!(stats.get_requests, request_count - 1);
    let expected_manifests =
        u64::try_from(sample_count.checked_mul(2).ok_or("count overflow")? + 1)?;
    let expected_objects = u64::try_from(sample_count.checked_mul(18).ok_or("count overflow")?)?;
    let expected_response_bytes = prefetched_prefix_bytes
        .checked_mul(expected_manifests)
        .and_then(|bytes| bytes.checked_add(point_envelope.checked_mul(expected_objects)?))
        .ok_or("S3 query response byte count overflow")?;
    assert_eq!(
        u64::try_from(stats.response_body_bytes)?,
        expected_response_bytes
    );
    assert_eq!(u64::try_from(stats.request_body_bytes)?, pack.len());
    verify_stored_pack(server, &pack);
    drop(warm_readers);
    drop(store);

    println!(
        "retrieval_bench fixture=clustered_documents objects={} documents={} records_per_document={} payload_bytes={} envelope_bytes={} pack_bytes={} closure={} local_bytes_written={} local_store_file_bytes={} local_amplification_x={:.3} s3_storage_amplification_x={:.3} upload_bytes={} response_bytes={} query=point_and_8_record_document_range seed={BENCHMARK_SEED}",
        objects.len(),
        DOCUMENT_COUNT,
        RECORDS_PER_DOCUMENT,
        payload_bytes,
        envelope_bytes,
        pack.len(),
        hex(closure.as_bytes()),
        local_bytes_written,
        local_storage,
        local_storage as f64 / payload_bytes as f64,
        pack.len() as f64 / payload_bytes as f64,
        stats.request_body_bytes,
        stats.response_body_bytes,
    );
    Ok(())
}

fn run_local_range_once(
    reader: &mut ArtifactObjectReader,
    object: &TypedObject,
    offset: usize,
    started: Instant,
) -> Result<u128, String> {
    if reader.id() != object.id() {
        return Err("local object ID differed from fixture".into());
    }
    let mut bytes = [0_u8; RANGE_BYTES];
    let read = reader
        .read_payload_range(
            u64::try_from(offset).map_err(|error| error.to_string())?,
            &mut bytes,
        )
        .map_err(|error| format!("local range read: {error:?}"))?;
    let first_byte_ns = started.elapsed().as_nanos();
    if read != bytes.len() || !matches_oracle(&bytes, offset) {
        return Err("local concurrent range differed from fixture".into());
    }
    Ok(first_byte_ns)
}

fn run_s3_range_once(
    route: &super::S3PackRoute,
    pack: &S3RemotePack,
    object: &TypedObject,
    offset: usize,
    started: Instant,
) -> Result<u128, String> {
    let checked = route
        .get_object(
            pack,
            object.id(),
            super::fence(),
            &backend_store::RelationAdmissionRegistry::default(),
        )
        .map_err(|error| format!("S3 object GET: {error:?}"))?;
    if checked.id() != object.id() {
        return Err("S3 object ID differed from fixture".into());
    }
    let mut envelope = checked
        .open_envelope()
        .map_err(|error| format!("open checked S3 extent: {error:?}"))?;
    envelope
        .seek(SeekFrom::Start(
            u64::try_from(OBJECT_HEADER_BYTES + offset).map_err(|error| error.to_string())?,
        ))
        .map_err(|error| error.to_string())?;
    let mut bytes = [0_u8; RANGE_BYTES];
    envelope
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    let first_byte_ns = started.elapsed().as_nanos();
    if !matches_oracle(&bytes, offset) {
        return Err("S3 concurrent range differed from fixture".into());
    }
    Ok(first_byte_ns)
}

fn parallel_local_batches(
    concurrency: usize,
    sample_count: usize,
    object: &TypedObject,
    sink: &backend_store::ArtifactSink,
    offset: usize,
) -> Result<Vec<Timing>, Box<dyn std::error::Error>> {
    let mut readers = (0..concurrency)
        .map(|_| {
            sink.open_object(object_id(object))
                .map_err(|error| std::io::Error::other(format!("backend-store: {error:?}")))?
                .ok_or_else(|| std::io::Error::other("concurrent local object missing"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let barrier = Arc::new(Barrier::new(concurrency + 1));
    let (sender, receiver) = mpsc::channel();
    let mut complete_ns = Vec::with_capacity(sample_count);
    let mut first_bytes = Vec::with_capacity(concurrency);
    thread::scope(|scope| {
        for mut reader in readers.drain(..) {
            let barrier = Arc::clone(&barrier);
            let sender = sender.clone();
            scope.spawn(move || {
                let mut failure = None;
                let mut first_bytes = Vec::with_capacity(sample_count);
                for _ in 0..sample_count {
                    barrier.wait();
                    if failure.is_none() {
                        match run_local_range_once(&mut reader, object, offset, Instant::now()) {
                            Ok(first_byte_ns) => first_bytes.push(first_byte_ns),
                            Err(error) => failure = Some(error),
                        }
                    }
                    barrier.wait();
                }
                let _ = sender.send((failure, first_bytes));
            });
        }
        drop(sender);
        for _ in 0..sample_count {
            let started = Instant::now();
            barrier.wait();
            barrier.wait();
            complete_ns.push(started.elapsed().as_nanos());
        }
        for (result, samples) in receiver {
            assert!(
                result.is_none(),
                "concurrent local reader failed: {result:?}"
            );
            first_bytes.push(samples);
        }
    });
    let mut timings = Vec::with_capacity(sample_count);
    for (sample_index, complete_ns) in complete_ns.into_iter().enumerate() {
        let first_byte_ns = first_bytes
            .iter()
            .filter_map(|samples| samples.get(sample_index).copied())
            .min()
            .ok_or("concurrent local sample returned no first-byte time")?;
        timings.push(Timing {
            first_byte_ns,
            complete_ns,
        });
    }
    Ok(timings)
}

fn parallel_s3_batches(
    concurrency: usize,
    sample_count: usize,
    object: &TypedObject,
    pack: &ImmutableS3Pack,
    offset: usize,
) -> Result<(Vec<Timing>, Vec<super::LoopbackServer>), Box<dyn std::error::Error>> {
    let mut servers = Vec::with_capacity(concurrency);
    let mut workers = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let server = super::LoopbackServer::start(
            super::ServerMode::Store,
            3usize
                .checked_add(sample_count)
                .ok_or("request count overflow")?,
            None,
        );
        let route = super::S3PackRoute::new(server.route_with_max(PACK_LIMIT_BYTES));
        let upload = super::pack_upload_capability(&server, pack, super::fence());
        route.put_pack(pack, &upload, super::fence(), None)?;
        let read = super::pack_read_capability(&server, pack, super::fence(), true);
        let remote = route.open_pack(&read, super::fence())?;
        servers.push(server);
        workers.push((route, remote));
    }

    let barrier = Arc::new(Barrier::new(concurrency + 1));
    let (sender, receiver) = mpsc::channel();
    let mut complete_ns = Vec::with_capacity(sample_count);
    let mut first_bytes = Vec::with_capacity(concurrency);
    thread::scope(|scope| {
        for (route, remote) in workers {
            let barrier = Arc::clone(&barrier);
            let sender = sender.clone();
            scope.spawn(move || {
                let mut failure = None;
                let mut first_bytes = Vec::with_capacity(sample_count);
                for _ in 0..sample_count {
                    barrier.wait();
                    if failure.is_none() {
                        match run_s3_range_once(&route, &remote, object, offset, Instant::now()) {
                            Ok(first_byte_ns) => first_bytes.push(first_byte_ns),
                            Err(error) => failure = Some(error),
                        }
                    }
                    barrier.wait();
                }
                let _ = sender.send((failure, first_bytes));
            });
        }
        drop(sender);
        for _ in 0..sample_count {
            let started = Instant::now();
            barrier.wait();
            barrier.wait();
            complete_ns.push(started.elapsed().as_nanos());
        }
        for (result, samples) in receiver {
            assert!(result.is_none(), "concurrent S3 reader failed: {result:?}");
            first_bytes.push(samples);
        }
    });
    let mut timings = Vec::with_capacity(sample_count);
    for (sample_index, complete_ns) in complete_ns.into_iter().enumerate() {
        let first_byte_ns = first_bytes
            .iter()
            .filter_map(|samples| samples.get(sample_index).copied())
            .min()
            .ok_or("concurrent S3 sample returned no first-byte time")?;
        timings.push(Timing {
            first_byte_ns,
            complete_ns,
        });
    }
    Ok((timings, servers))
}

fn concurrent_loopback_route(
    server: &concurrent_loopback::ConcurrentLoopbackServer,
) -> Result<super::S3PackRoute, Box<dyn std::error::Error>> {
    let endpoint = crate::S3Endpoint::loopback_http(server.origin())?;
    let config = crate::S3RouteConfig::new([endpoint], PACK_LIMIT_BYTES, 3, Duration::ZERO)?;
    Ok(super::S3PackRoute::new(crate::S3ObjectRoute::new(config)))
}

fn concurrent_pack_upload_capability(
    server: &concurrent_loopback::ConcurrentLoopbackServer,
    pack: &ImmutableS3Pack,
) -> crate::PackUploadCapability {
    let url = format!(
        "{}/bucket/object?X-Amz-Signature=secret-token&X-Amz-SignedHeaders=host%3Bif-none-match%3Bx-amz-checksum-sha256",
        server.origin()
    );
    crate::PackUploadCapability::new(
        url,
        pack.pack_id(),
        pack.layout_id(),
        pack.len(),
        pack.manifest_bytes(),
        *pack.sha256(),
        pack.manifest_sha256(),
        super::expires_later(),
        super::fence(),
        true,
        true,
    )
}

fn concurrent_pack_read_capability(
    server: &concurrent_loopback::ConcurrentLoopbackServer,
    pack: &ImmutableS3Pack,
) -> crate::PackReadCapability {
    let url = format!(
        "{}/bucket/object?X-Amz-Signature=secret-token&X-Amz-SignedHeaders=host",
        server.origin()
    );
    crate::PackReadCapability::new(
        url,
        pack.pack_id(),
        pack.layout_id(),
        pack.len(),
        pack.manifest_bytes(),
        *pack.sha256(),
        pack.manifest_sha256(),
        super::expires_later(),
        super::fence(),
        true,
    )
}

/// Measures the same route and object oracle as `parallel_s3_batches`, with
/// all readers sharing one S3-shaped server whose bounded worker pool can
/// service requests concurrently. The historical one-server-per-reader lane
/// above remains intact for comparison.
fn parallel_s3_batches_concurrent_server(
    concurrency: usize,
    sample_count: usize,
    object: &TypedObject,
    pack: &ImmutableS3Pack,
    offset: usize,
) -> Result<(Vec<Timing>, concurrent_loopback::ConcurrentLoopbackStats), Box<dyn std::error::Error>>
{
    let server = concurrent_loopback::ConcurrentLoopbackServer::start(CONCURRENT_LOOPBACK_WORKERS)?;
    let upload_route = concurrent_loopback_route(&server)?;
    let upload = concurrent_pack_upload_capability(&server, pack);
    upload_route.put_pack(pack, &upload, super::fence(), None)?;
    drop(upload_route);
    let read = concurrent_pack_read_capability(&server, pack);
    let mut readers = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let route = concurrent_loopback_route(&server)?;
        let remote = route.open_pack(&read, super::fence())?;
        readers.push((route, remote));
    }

    let barrier = Arc::new(Barrier::new(concurrency + 1));
    let (sender, receiver) = mpsc::channel();
    let mut complete_ns = Vec::with_capacity(sample_count);
    let mut first_bytes = Vec::with_capacity(concurrency);
    thread::scope(|scope| {
        for (route, remote) in readers {
            let barrier = Arc::clone(&barrier);
            let sender = sender.clone();
            scope.spawn(move || {
                let mut failure = None;
                let mut first_bytes = Vec::with_capacity(sample_count);
                for _ in 0..sample_count {
                    barrier.wait();
                    if failure.is_none() {
                        match run_s3_range_once(&route, &remote, object, offset, Instant::now()) {
                            Ok(first_byte_ns) => first_bytes.push(first_byte_ns),
                            Err(error) => failure = Some(error),
                        }
                    }
                    barrier.wait();
                }
                let _ = sender.send((failure, first_bytes));
            });
        }
        drop(sender);
        for _ in 0..sample_count {
            let started = Instant::now();
            barrier.wait();
            barrier.wait();
            complete_ns.push(started.elapsed().as_nanos());
        }
        for (result, samples) in receiver {
            assert!(
                result.is_none(),
                "concurrent loopback S3 reader failed: {result:?}"
            );
            first_bytes.push(samples);
        }
    });

    let mut timings = Vec::with_capacity(sample_count);
    for (sample_index, complete_ns) in complete_ns.into_iter().enumerate() {
        let first_byte_ns = first_bytes
            .iter()
            .filter_map(|samples| samples.get(sample_index).copied())
            .min()
            .ok_or("concurrent loopback S3 sample returned no first-byte time")?;
        timings.push(Timing {
            first_byte_ns,
            complete_ns,
        });
    }

    let stats = server.stats();
    let expected_gets = concurrency
        .checked_mul(sample_count.checked_add(2).ok_or("GET count overflow")?)
        .ok_or("GET count overflow")?;
    let expected_requests = expected_gets
        .checked_add(1)
        .ok_or("request count overflow")?;
    let page_bytes = u64::from(
        pack.manifest()
            .page_for_object(object.id())
            .ok_or("single object page missing")?
            .1
            .bytes(),
    );
    let envelope_bytes = u64::try_from(OBJECT_HEADER_BYTES + object.bytes().len())?;
    let setup_response_bytes = u64::from(pack.manifest_bytes())
        .checked_add(page_bytes)
        .and_then(|bytes| bytes.checked_mul(u64::try_from(concurrency).ok()?))
        .ok_or("setup response byte count overflow")?;
    let query_count = sample_count
        .checked_mul(concurrency)
        .ok_or("query count overflow")?;
    let query_response_bytes = envelope_bytes
        .checked_mul(u64::try_from(query_count)?)
        .ok_or("query response byte count overflow")?;
    let expected_response_bytes = setup_response_bytes
        .checked_add(query_response_bytes)
        .ok_or("response byte count overflow")?;
    assert_eq!(stats.requests, expected_requests);
    assert_eq!(stats.put_requests, 1);
    assert_eq!(stats.get_requests, expected_gets);
    assert_eq!(u64::try_from(stats.request_body_bytes)?, pack.len());
    assert_eq!(
        u64::try_from(stats.response_body_bytes)?,
        expected_response_bytes
    );
    assert!(stats.peak_active_connections <= CONCURRENT_LOOPBACK_WORKERS);
    drop(server);
    Ok((timings, stats))
}

fn benchmark_concurrency(sample_count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let object = benchmark_object(901, 64 * 1024);
    let objects = vec![object.clone()];
    let directory = BenchDirectory::new("concurrency")?;
    let (store, closure, budget, _local_bytes_written) =
        persist_local_objects(directory.path(), &objects)?;
    let sink = store.artifact_sink(budget);
    let pack = build_pack(&objects)?;
    let offset = 12_345usize;
    for concurrency in [1usize, 4, 16] {
        let local = parallel_local_batches(concurrency, sample_count, &object, &sink, offset)?;
        let remote = parallel_s3_batches(concurrency, sample_count, &object, &pack, offset)?;
        report_parallel(
            &format!("concurrency.local.warm_range.c={concurrency}"),
            &local,
            u64::try_from(RANGE_BYTES * concurrency)?,
        );
        report_parallel(
            &format!("concurrency.s3_loopback.warm_pack.c={concurrency}"),
            &remote.0,
            u64::try_from(
                (OBJECT_HEADER_BYTES + object.bytes().len())
                    .checked_mul(concurrency)
                    .ok_or("parallel transfer bytes overflow")?,
            )?,
        );
        let mut upload_bytes = 0_u64;
        let mut response_bytes = 0_u64;
        for server in remote.1 {
            let stats = server.stats();
            assert_eq!(stats.requests, sample_count + 3);
            assert_eq!(stats.put_requests, 1);
            assert_eq!(stats.get_requests, sample_count + 2);
            assert_eq!(u64::try_from(stats.request_body_bytes)?, pack.len());
            assert_eq!(
                u64::try_from(stats.response_body_bytes)?,
                u64::from(pack.manifest_bytes())
                    + u64::from(
                        pack.manifest()
                            .page_for_object(object.id())
                            .ok_or("single object page missing")?
                            .bytes(),
                    )
                    + u64::try_from(sample_count * (OBJECT_HEADER_BYTES + object.bytes().len()))?
            );
            upload_bytes = upload_bytes
                .checked_add(u64::try_from(stats.request_body_bytes)?)
                .ok_or("parallel upload byte count overflow")?;
            response_bytes = response_bytes
                .checked_add(u64::try_from(stats.response_body_bytes)?)
                .ok_or("parallel response byte count overflow")?;
            verify_stored_pack(server, &pack);
        }
        println!(
            "retrieval_bench traffic=concurrency readers={concurrency} upload_bytes={upload_bytes} response_bytes={response_bytes} resent_bytes=0 retries=0"
        );

        let (concurrent, concurrent_stats) = parallel_s3_batches_concurrent_server(
            concurrency,
            sample_count,
            &object,
            &pack,
            offset,
        )?;
        let envelope_bytes = u64::try_from(OBJECT_HEADER_BYTES + object.bytes().len())?;
        report_parallel(
            &format!("concurrency.s3_loopback.shared_server.warm_range.c={concurrency}"),
            &concurrent,
            envelope_bytes
                .checked_mul(u64::try_from(concurrency)?)
                .ok_or("shared-server read bytes overflow")?,
        );
        let setup_gets = concurrency
            .checked_mul(2)
            .ok_or("shared-server setup GET count overflow")?;
        let query_gets = concurrency
            .checked_mul(sample_count)
            .ok_or("shared-server query GET count overflow")?;
        let setup_response_bytes = u64::from(pack.manifest_bytes())
            .checked_add(u64::from(
                pack.manifest()
                    .page_for_object(object.id())
                    .ok_or("single object page missing")?
                    .1
                    .bytes(),
            ))
            .and_then(|bytes| bytes.checked_mul(u64::try_from(concurrency).ok()?))
            .ok_or("shared-server setup bytes overflow")?;
        let query_response_bytes = envelope_bytes
            .checked_mul(u64::try_from(query_gets)?)
            .ok_or("shared-server query bytes overflow")?;
        println!(
            "retrieval_bench traffic=concurrency_shared_server readers={concurrency} server_workers={CONCURRENT_LOOPBACK_WORKERS} accepted_tcp_connections={} peak_active_connections={} put_requests={} setup_gets={setup_gets} query_gets={query_gets} total_gets={} upload_bytes={} setup_response_bytes={setup_response_bytes} query_response_bytes={query_response_bytes} response_bytes={} process_peak_rss_bytes={}",
            concurrent_stats.accepted_tcp_connections,
            concurrent_stats.peak_active_connections,
            concurrent_stats.put_requests,
            concurrent_stats.get_requests,
            concurrent_stats.request_body_bytes,
            concurrent_stats.response_body_bytes,
            peak_rss_bytes().map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        );
    }
    drop(store);
    println!(
        "retrieval_bench fixture=concurrency payload_bytes={} envelope_bytes={} pack_bytes={} closure={} range_bytes={} offset={} readers=1,4,16 samples_per_reader={sample_count} seed={BENCHMARK_SEED} historical_s3_server_model=one_real_loopback_tcp_server_per_reader additional_s3_server_model=one_shared_bounded_concurrent_loopback_server",
        object.bytes().len(),
        OBJECT_HEADER_BYTES + object.bytes().len(),
        pack.len(),
        hex(closure.as_bytes()),
        RANGE_BYTES,
        offset,
    );
    Ok(())
}

fn report_parallel(label: &str, timings: &[Timing], bytes_per_batch: u64) {
    let count = timings.len();
    let first = |p| percentile(timings.iter().map(|sample| sample.first_byte_ns), count, p);
    let complete = |p| percentile(timings.iter().map(|sample| sample.complete_ns), count, p);
    println!(
        "retrieval_bench lane={label} samples={count} first_byte_ns_p50={} first_byte_ns_p95={} first_byte_ns_p99={} batch_complete_ns_p50={} batch_complete_ns_p95={} batch_complete_ns_p99={} read_bytes_per_batch={bytes_per_batch}",
        first(50),
        first(95),
        first(99),
        complete(50),
        complete(95),
        complete(99)
    );
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[test]
#[ignore = "long-running end-to-end retrieval measurement; run explicitly with --ignored"]
fn retrieval_benchmark_reports_latency_bytes_and_amplification()
-> Result<(), Box<dyn std::error::Error>> {
    if env::var_os(COLD_WORKER_ENV).is_some() {
        run_local_cold_worker()?;
        return Ok(());
    }
    let sample_count = samples();
    let test_filter = benchmark_test_filter()?;
    println!(
        "retrieval_bench protocol=1 samples={sample_count} seed={BENCHMARK_SEED} os_page_cache=not_flushed allocations=not_instrumented s3_kind=in_memory_tcp_loopback"
    );
    for payload_len in LARGE_OBJECT_SIZES {
        benchmark_one_size(payload_len, sample_count, &test_filter)?;
    }
    benchmark_clustered_documents(sample_count, &test_filter)?;
    benchmark_concurrency(sample_count)?;
    println!(
        "retrieval_bench peak_rss_bytes={}",
        peak_rss_bytes().map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );
    println!("retrieval_bench allocation_counts=unavailable_without_allocator_instrumentation");
    Ok(())
}

fn run_local_cold_worker() -> Result<(), Box<dyn std::error::Error>> {
    if env::var_os(COLD_QUERY_KIND_ENV).is_some() {
        return run_local_cold_query_worker();
    }
    let root = env::var_os(COLD_ROOT_ENV).ok_or("missing cold worker store path")?;
    let object_index = env::var(COLD_INDEX_ENV)?.parse::<usize>()?;
    let payload_len = env::var(COLD_SIZE_ENV)?.parse::<usize>()?;
    let object = benchmark_object(object_index, payload_len);
    let budget = local_budget(std::slice::from_ref(&object));
    let started = Instant::now();
    let store = store_result(FileStore::open(
        PathBuf::from(root),
        usize::try_from(PACK_LIMIT_BYTES)?,
    ))?;
    let mut reader = store
        .artifact_sink(budget)
        .open_object(object_id(&object))
        .map_err(StoreBenchError)?
        .ok_or("cold process could not reopen local CAS object")?;
    let timing = read_local_range(&mut reader, &object, 0, payload_len, started)?;
    println!(
        "BENCH_LOCAL_COLD_NS {} {}",
        timing.first_byte_ns, timing.complete_ns
    );
    Ok(())
}

fn run_local_cold_query_worker() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::var_os(COLD_ROOT_ENV).ok_or("missing cold query store path")?;
    let query_kind = env::var(COLD_QUERY_KIND_ENV)?;
    let query_index = env::var(COLD_QUERY_INDEX_ENV)?.parse::<usize>()?;
    let sample_count = samples();
    let (objects, documents) = clustered_document_fixture();
    let mut positions = HashMap::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        positions.insert(*object.id().as_bytes(), index);
    }
    let (point_queries, range_queries) = make_queries(&documents, &positions, sample_count);
    let queries = match query_kind.as_str() {
        "point" => &point_queries,
        "range" => &range_queries,
        _ => return Err("unknown cold clustered query kind".into()),
    };
    let query = queries
        .get(query_index)
        .ok_or("cold clustered query index outside fixture")?;
    let budget = local_budget(&objects);
    let started = Instant::now();
    let store = store_result(FileStore::open(
        PathBuf::from(root),
        usize::try_from(PACK_LIMIT_BYTES)?,
    ))?;
    let sink = store.artifact_sink(budget);
    let timing = run_local_query(&sink, &objects, query, started)?;
    println!(
        "BENCH_LOCAL_COLD_NS {} {}",
        timing.first_byte_ns, timing.complete_ns
    );
    Ok(())
}

fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = fs::read_to_string("/proc/self/status").ok()?;
        let kilobytes = status.lines().find_map(|line| {
            line.strip_prefix("VmHWM:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })?;
        return kilobytes.checked_mul(1024);
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
