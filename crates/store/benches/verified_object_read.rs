//! Compares the legacy owned object read plus a producer payload clone with
//! the callback-lent, GC-pinned borrowed read path.

#![allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::unwrap_used
)]

use std::{
    env,
    error::Error,
    fs,
    hint::black_box,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use allocation_counter::{AllocationInfo, measure};
use backend_store::{FileStore, GcPinGuard, ObjectId, StoreError, TypedObject};
use backend_version::{ObjectKey, Schema};

const OBJECT_SIZES: [usize; 2] = [1024, 1024 * 1024];
const WARMUP_COUNT: usize = 8;
const RSS_WINDOW: Duration = Duration::from_millis(250);

struct BytesSchema;

impl Schema for BytesSchema {
    const DOMAIN: u8 = 0xf2;
    const TYPE: u16 = 0x5102;
    type Value = Vec<u8>;

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

#[derive(Clone, Copy, Debug)]
enum ReadMode {
    Owned,
    OwnedThenPayloadCopy,
    BorrowedPinned,
    BorrowedConvenience,
}

impl ReadMode {
    const ALL: [Self; 4] = [
        Self::Owned,
        Self::OwnedThenPayloadCopy,
        Self::BorrowedPinned,
        Self::BorrowedConvenience,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Owned => "legacy_owned",
            Self::OwnedThenPayloadCopy => "legacy_owned_plus_producer_vec_copy",
            Self::BorrowedPinned => "borrowed_pinned",
            Self::BorrowedConvenience => "borrowed_convenience_pin_per_call",
        }
    }
}

struct Fixture {
    store: FileStore,
    id: ObjectId,
    pin: Option<GcPinGuard>,
    root: PathBuf,
}

fn main() -> Result<(), Box<dyn Error>> {
    let samples = env::args()
        .nth(1)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(101);
    if samples == 0 {
        return Err("sample count must be positive".into());
    }

    println!(
        "verified_object_read protocol=1 samples={samples} warmups={WARMUP_COUNT} rss_window_ms={} allocation_scope=allocation_counter_current_thread preexisting_fixture_allocations_excluded first_read_cache_state=uncontrolled rss_peak=sampled_ps_process_rss explicit_producer_payload_copy_bytes_are_reported_separately=true",
        RSS_WINDOW.as_millis()
    );
    for size in OBJECT_SIZES {
        for mode in ReadMode::ALL {
            let fixture = make_fixture(size, mode)?;
            let envelope_bytes = fs::metadata(
                fixture
                    .store
                    .root()
                    .join("objects")
                    .join(format!("{}.object", hex(fixture.id.as_bytes()))),
            )?
            .len();

            let first_start = Instant::now();
            run_once(mode, &fixture.store, fixture.id, fixture.pin.as_ref())
                .map_err(store_io_error)?;
            let first_read_ns = first_start.elapsed().as_nanos();

            for _ in 0..WARMUP_COUNT {
                run_once(mode, &fixture.store, fixture.id, fixture.pin.as_ref())
                    .map_err(store_io_error)?;
            }
            let allocation = allocation_sample(mode, &fixture).map_err(store_io_error)?;
            let latencies = latency_samples(mode, &fixture, samples).map_err(store_io_error)?;
            let (rss_before, rss_peak, rss_after) =
                sampled_rss(mode, &fixture).map_err(store_io_error)?;
            report(
                size,
                envelope_bytes,
                mode,
                first_read_ns,
                &latencies,
                allocation,
                rss_before,
                rss_peak,
                rss_after,
            );
            drop(fixture.pin);
            let _ = fs::remove_dir_all(fixture.root);
        }
    }
    println!(
        "verified_object_read limitations first_read_is_not_a_cold_page_cache_measurement=true owned_decode_payload_copy_bytes=payload_length explicit_producer_copy_bytes=owned_bytes_to_vec_step borrowed_total_payload_copy_bytes=0"
    );
    Ok(())
}

fn make_fixture(size: usize, mode: ReadMode) -> Result<Fixture, Box<dyn Error>> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = env::temp_dir().join(format!(
        "backend-store-verified-read-{}-{}-{nonce}",
        std::process::id(),
        mode.label()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = FileStore::open(&root, 2 * 1024 * 1024).map_err(store_io_error)?;
    let value = vec![0x5a; size];
    let key = ObjectKey::<BytesSchema>::from_value(&value);
    let object = TypedObject::from_value(&key, &value);
    let id = store.write_object(&object).map_err(store_io_error)?;
    drop(object);
    drop(value);
    let pin = if matches!(mode, ReadMode::BorrowedPinned) {
        Some(store.pin_garbage_collection().map_err(store_io_error)?)
    } else {
        None
    };
    Ok(Fixture {
        store,
        id,
        pin,
        root,
    })
}

fn run_once(
    mode: ReadMode,
    store: &FileStore,
    id: ObjectId,
    pin: Option<&GcPinGuard>,
) -> Result<(), StoreError> {
    match mode {
        ReadMode::Owned => {
            let object = store.read_object(id)?;
            black_box(object.bytes());
        }
        ReadMode::OwnedThenPayloadCopy => {
            let object = store.read_object(id)?;
            let second_payload = object.bytes().to_vec();
            black_box(second_payload);
        }
        ReadMode::BorrowedPinned => {
            let pin = pin.ok_or(StoreError::Corrupt)?;
            store.with_verified_object_pinned(pin, id, |object| {
                black_box(object.bytes());
                Ok(())
            })?;
        }
        ReadMode::BorrowedConvenience => {
            store.with_verified_object(id, |object| {
                black_box(object.bytes());
                Ok(())
            })?;
        }
    }
    Ok(())
}

fn allocation_sample(mode: ReadMode, fixture: &Fixture) -> Result<AllocationInfo, StoreError> {
    let mut result = None;
    let info = measure(|| {
        result = Some(run_once(
            mode,
            &fixture.store,
            fixture.id,
            fixture.pin.as_ref(),
        ));
    });
    result.expect("allocation measurement executed")?;
    Ok(info)
}

fn latency_samples(
    mode: ReadMode,
    fixture: &Fixture,
    samples: usize,
) -> Result<Vec<u128>, StoreError> {
    let mut latency = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        run_once(mode, &fixture.store, fixture.id, fixture.pin.as_ref())?;
        latency.push(start.elapsed().as_nanos());
    }
    latency.sort_unstable();
    Ok(latency)
}

fn sampled_rss(
    mode: ReadMode,
    fixture: &Fixture,
) -> Result<(Option<u64>, Option<u64>, Option<u64>), StoreError> {
    let before = current_rss_bytes();
    let stop = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicU64::new(before.unwrap_or_default()));
    let observed = Arc::new(AtomicBool::new(before.is_some()));
    let sampler_stop = Arc::clone(&stop);
    let sampler_peak = Arc::clone(&peak);
    let sampler_observed = Arc::clone(&observed);
    let sampler = thread::spawn(move || {
        while !sampler_stop.load(Ordering::Relaxed) {
            if let Some(current) = current_rss_bytes() {
                sampler_peak.fetch_max(current, Ordering::Relaxed);
                sampler_observed.store(true, Ordering::Relaxed);
            }
            thread::sleep(Duration::from_millis(5));
        }
    });

    let start = Instant::now();
    let mut result = Ok(());
    while start.elapsed() < RSS_WINDOW {
        if let Err(error) = run_once(mode, &fixture.store, fixture.id, fixture.pin.as_ref()) {
            result = Err(error);
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    sampler.join().expect("RSS sampler thread");
    result?;
    Ok((
        before,
        observed
            .load(Ordering::Relaxed)
            .then(|| peak.load(Ordering::Relaxed)),
        current_rss_bytes(),
    ))
}

fn current_rss_bytes() -> Option<u64> {
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let kibibytes = std::str::from_utf8(&output.stdout)
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kibibytes.checked_mul(1024)
}

fn store_io_error(error: StoreError) -> std::io::Error {
    std::io::Error::other(format!("FileStore benchmark failed: {error:?}"))
}

fn report(
    size: usize,
    envelope_bytes: u64,
    mode: ReadMode,
    first_read_ns: u128,
    latencies: &[u128],
    allocation: AllocationInfo,
    rss_before: Option<u64>,
    rss_peak: Option<u64>,
    rss_after: Option<u64>,
) {
    let p50 = percentile(latencies, 50);
    let p95 = percentile(latencies, 95);
    let p99 = percentile(latencies, 99);
    let explicit_copy = if matches!(mode, ReadMode::OwnedThenPayloadCopy) {
        size
    } else {
        0
    };
    let decode_copy = if matches!(mode, ReadMode::Owned | ReadMode::OwnedThenPayloadCopy) {
        size
    } else {
        0
    };
    let total_payload_copies = decode_copy + explicit_copy;
    println!(
        "verified_object_read sample={} payload_bytes={size} envelope_bytes={envelope_bytes} mode={} first_read_ns={first_read_ns} warm_p50_ns={p50} warm_p95_ns={p95} warm_p99_ns={p99} allocations={} allocated_bytes={} max_live_allocated_bytes={} owned_decode_payload_copy_bytes={decode_copy} explicit_producer_payload_copy_bytes={explicit_copy} total_payload_copy_bytes={total_payload_copies} rss_before_bytes={} rss_peak_sampled_bytes={} rss_after_bytes={}",
        latencies.len(),
        mode.label(),
        allocation.count_total,
        allocation.bytes_total,
        allocation.bytes_max,
        format_metric(rss_before),
        format_metric(rss_peak),
        format_metric(rss_after),
    );
}

fn percentile(values: &[u128], percentile: usize) -> u128 {
    let rank = values.len().saturating_mul(percentile).saturating_add(99) / 100;
    values[rank.saturating_sub(1).min(values.len() - 1)]
}

fn format_metric(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
}

fn hex(bytes: &[u8; 32]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("write to string");
    }
    output
}
