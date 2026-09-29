//! End-to-end jumbo UTF-8 writer comparison for the chunk-level ASCII scan.
#![allow(clippy::expect_used, clippy::print_stdout, clippy::unwrap_used)]

use std::{convert::Infallible, env, hint::black_box, io::Cursor, time::Instant};

use allocation_counter::{AllocationInfo, measure};
use backend_semantic::ir::{
    JumboRopeLimits, JumboRopeNode, JumboRopeObjectSink, JumboRopeWriteReceipt, JumboValueContext,
    JumboValueEncoding, JumboValueFamily, MAX_SEMANTIC_SEGMENT_BYTES, write_jumbo_value,
    write_jumbo_value_from_reader,
};

const DEFAULT_SAMPLES: usize = 5;
const DEFAULT_WARMUPS: usize = 1;
const DEFAULT_REPEATS: usize = 1;

#[derive(Clone, Copy)]
struct Options {
    samples: usize,
    warmups: usize,
    repeats: usize,
}

#[derive(Clone, Copy, Default)]
struct CountingSink {
    leaves: u64,
    interiors: u64,
    bytes: u64,
    checksum: u64,
}

impl JumboRopeObjectSink for CountingSink {
    type Error = Infallible;

    fn write_leaf(
        &mut self,
        leaf: backend_semantic::ir::JumboRopeLeafRef<'_>,
    ) -> Result<(), Self::Error> {
        self.leaves += 1;
        self.bytes += leaf.bytes().len() as u64;
        self.checksum ^=
            u64::from_le_bytes(leaf.id().as_bytes()[..8].try_into().expect("ID prefix"));
        Ok(())
    }

    fn write_interior(&mut self, node: &JumboRopeNode) -> Result<(), Self::Error> {
        self.interiors += 1;
        self.checksum ^=
            u64::from_le_bytes(node.id().as_bytes()[..8].try_into().expect("ID prefix"));
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct Summary {
    p50_ns: u128,
    p95_ns: u128,
    allocations_per_write: u64,
    allocated_bytes_per_write: u64,
    max_live_bytes_p95: u64,
}

fn option(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn options() -> Options {
    Options {
        samples: option("JUMBO_ASCII_SAMPLES", DEFAULT_SAMPLES),
        warmups: option("JUMBO_ASCII_WARMUPS", DEFAULT_WARMUPS),
        repeats: option("JUMBO_ASCII_REPEATS", DEFAULT_REPEATS),
    }
}

fn ascii_document(target: usize) -> Vec<u8> {
    let paragraph = b"# Design notes\n\nThe writer preserves canonical byte order while splitting large documentation into bounded content-addressed leaves. Each leaf is admitted to the object store before its parent descriptor becomes visible. Repeated writes produce the same identities and tree shape.\n\n";
    repeat_to_jumbo_size(paragraph, target)
}

fn mixed_unicode_document(target: usize) -> Vec<u8> {
    let paragraph = "# Unicode notes\n\nRust supports café text, 東京 references, mathematical symbols ∑ and λ, and emoji 🧪 without changing the canonical byte stream. The writer must keep UTF-8 validation correct across arbitrary input blocks.\n\n";
    repeat_to_jumbo_size(paragraph.as_bytes(), target)
}

fn repeat_to_jumbo_size(paragraph: &[u8], target: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(target);
    while bytes.len() < target {
        bytes.extend_from_slice(paragraph);
    }
    bytes
}

fn write(bytes: &[u8], stream: bool) -> (JumboRopeWriteReceipt, CountingSink) {
    let mut sink = CountingSink::default();
    let context = JumboValueContext::new(
        [0x5a; 32],
        JumboValueFamily::Documentation,
        3,
        JumboValueEncoding::Utf8,
    );
    let receipt = if stream {
        let mut reader = Cursor::new(bytes);
        write_jumbo_value_from_reader(context, &mut reader, JumboRopeLimits::default(), &mut sink)
    } else {
        write_jumbo_value(context, bytes, JumboRopeLimits::default(), &mut sink)
    }
    .expect("fixture is valid UTF-8 and within default limits");
    (receipt, sink)
}

fn measure_case(bytes: &[u8], stream: bool, options: Options) -> Summary {
    for _ in 0..options.warmups {
        black_box(write(black_box(bytes), stream));
    }

    let mut elapsed = Vec::with_capacity(options.samples);
    let mut allocations = Vec::with_capacity(options.samples);
    let mut allocated_bytes = Vec::with_capacity(options.samples);
    let mut max_live_bytes = Vec::with_capacity(options.samples);
    for _ in 0..options.samples {
        let mut elapsed_ns = 0_u128;
        let allocation: AllocationInfo = measure(|| {
            let start = Instant::now();
            for _ in 0..options.repeats {
                black_box(write(black_box(bytes), stream));
            }
            elapsed_ns = start.elapsed().as_nanos()
                / u128::try_from(options.repeats).expect("repeat count fits u128");
        });
        elapsed.push(elapsed_ns);
        allocations.push(allocation.count_total);
        allocated_bytes.push(allocation.bytes_total);
        max_live_bytes.push(allocation.bytes_max);
    }
    elapsed.sort_unstable();
    allocations.sort_unstable();
    allocated_bytes.sort_unstable();
    max_live_bytes.sort_unstable();
    let p95_index = (options.samples * 95).div_ceil(100).saturating_sub(1);
    Summary {
        p50_ns: elapsed[options.samples / 2],
        p95_ns: elapsed[p95_index],
        allocations_per_write: allocations[options.samples / 2]
            / u64::try_from(options.repeats).expect("repeat count fits u64"),
        allocated_bytes_per_write: allocated_bytes[options.samples / 2]
            / u64::try_from(options.repeats).expect("repeat count fits u64"),
        max_live_bytes_p95: max_live_bytes[p95_index],
    }
}

fn report(name: &str, bytes: &[u8], stream: bool, options: Options, summary: Summary) {
    let throughput_mib_s =
        bytes.len() as f64 * 1_000_000_000.0 / (summary.p50_ns as f64 * 1024.0 * 1024.0);
    println!(
        "jumbo_utf8_ascii\tmode={}\tcase={}\tinput={}\tbytes={}\tsamples={}\twarmups={}\trepeats={}\tp50_ns={}\tp95_ns={}\tthroughput_mib_s={:.2}\tallocations_per_write={}\tallocated_bytes_per_write={}\tmax_live_bytes_p95={}",
        if cfg!(feature = "jumbo-utf8-ascii-scalar") {
            "scalar"
        } else {
            "ascii-scan"
        },
        name,
        if stream { "reader-64k" } else { "borrowed" },
        bytes.len(),
        options.samples,
        options.warmups,
        options.repeats,
        summary.p50_ns,
        summary.p95_ns,
        throughput_mib_s,
        summary.allocations_per_write,
        summary.allocated_bytes_per_write,
        summary.max_live_bytes_p95,
    );
}

fn main() {
    let options = options();
    for target in [
        MAX_SEMANTIC_SEGMENT_BYTES + 64 * 1024,
        2 * MAX_SEMANTIC_SEGMENT_BYTES,
        4 * MAX_SEMANTIC_SEGMENT_BYTES,
    ] {
        let cases = [
            ("ascii-heavy-doc", ascii_document(target)),
            ("mixed-unicode-doc", mixed_unicode_document(target)),
        ];
        for (name, bytes) in &cases {
            for stream in [false, true] {
                let (receipt, sink) = write(bytes, stream);
                assert_eq!(sink.bytes, bytes.len() as u64);
                assert_eq!(sink.leaves, receipt.metrics().leaf_count());
                black_box((sink.checksum, sink.interiors));
                report(
                    name,
                    bytes,
                    stream,
                    options,
                    measure_case(bytes, stream, options),
                );
            }
        }
    }
}
