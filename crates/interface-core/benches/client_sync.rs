//! Standalone local client selection benchmark.
//!
//! The optimized lane calls the production `ClientIndex::select_local` API after a complete
//! manifest has been accepted and its alternating immutable segments have been admitted as
//! resident. The baseline repeats linear manifest and residence scans for each demand. Fixture
//! generation, manifest validation, client admission, residence recording, and all allocation
//! happen before timing.
//!
//! Run reproducibly with:
//!
//! ```text
//! cargo bench --locked --offline -p interface-core --bench client_sync
//! ```
//!
//! The bench target must remain `harness = false` in `interface/core/Cargo.toml`.

use core::num::NonZeroU64;
use std::time::Instant;

use backend_version::{
    ContentId, GenerationId, IndexExactSegmentDomain, IndexLexicalSegmentDomain,
    derive_index_snapshot,
};
use interface_core::{
    ClientIndex, ClientManifest, DemandSelection, LocalQueryTerminal, LocalSelection,
    ManifestEpoch, ManifestSegment, RemoteGeneration, RemoteManifest, SegmentDemand, SegmentId,
    SegmentRange, SelectionOutput, SelectionScratch, SyncCancellation,
};

const LANE_LEN: usize = 8;
const DEMANDS_PER_SEGMENT: usize = 16;
const DEMANDS_PER_SEGMENT_U64: u64 = 16;
const WARMUPS: usize = 20;
const SAMPLES: usize = 100;
const OPERATIONS_PER_SAMPLE: u64 = 100;
const FIXTURE_IDS: [u8; LANE_LEN] = [1, 2, 3, 4, 5, 6, 7, 8];

struct Fixture {
    exact: Vec<ManifestSegment>,
    lexical: Vec<ManifestSegment>,
    demands: Vec<SegmentDemand>,
    resident: Vec<interface_core::ResidentRange>,
    generation: GenerationId,
    snapshot: interface_core::IndexSnapshotId,
}

fn nonzero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap_or_else(|| panic!("benchmark fixture requires non-zero data"))
}

fn exact_id(value: u8) -> ContentId<IndexExactSegmentDomain> {
    ContentId::from_canonical_bytes(&[value])
}

fn lexical_id(value: u8) -> ContentId<IndexLexicalSegmentDomain> {
    ContentId::from_canonical_bytes(&[value])
}

fn fixture() -> Fixture {
    let exact_ids: Vec<_> = FIXTURE_IDS.into_iter().map(exact_id).collect();
    let lexical_ids: Vec<_> = FIXTURE_IDS.into_iter().map(lexical_id).collect();
    let mut exact_ids = exact_ids;
    let mut lexical_ids = lexical_ids;
    exact_ids.sort_unstable();
    lexical_ids.sort_unstable();
    exact_ids.reverse();
    lexical_ids.rotate_left(3);
    let exact = exact_ids
        .iter()
        .copied()
        .map(|id| ManifestSegment::exact(id, nonzero(1024)))
        .collect::<Vec<_>>();
    let lexical = lexical_ids
        .iter()
        .copied()
        .map(|id| ManifestSegment::lexical(id, nonzero(2048)))
        .collect::<Vec<_>>();
    let generation = GenerationId::from_digest([7; 32]);
    let snapshot = derive_index_snapshot(generation, &exact_ids, &lexical_ids)
        .unwrap_or_else(|_| panic!("benchmark fixture snapshot is representable"));
    let mut demand_segments = exact.iter().chain(&lexical).copied().collect::<Vec<_>>();
    demand_segments.sort_unstable_by_key(|segment| segment.id());
    let demands = demand_segments
        .iter()
        .flat_map(|segment| {
            (0..DEMANDS_PER_SEGMENT_U64).map(move |part| {
                let start = part.saturating_mul(64);
                SegmentDemand::new(
                    segment.id(),
                    SegmentRange::new(start, nonzero(64))
                        .unwrap_or_else(|_| panic!("fixture range")),
                )
            })
        })
        .collect();
    let resident = demand_segments
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 2 == 0)
        .map(|(_, segment)| interface_core::ResidentRange::new(segment.id(), segment.full_range()))
        .collect();
    Fixture {
        exact,
        lexical,
        demands,
        resident,
        generation,
        snapshot,
    }
}

fn segment_checksum(segment: SegmentId) -> u64 {
    let checksum_bytes = |tag: u64, bytes: &[u8]| {
        bytes.iter().fold(tag, |checksum, byte| {
            checksum.rotate_left(5) ^ u64::from(*byte)
        })
    };
    match segment {
        SegmentId::Exact(id) => checksum_bytes(0xE1, id.as_ref()),
        SegmentId::Lexical(id) => checksum_bytes(0x1E, id.as_ref()),
    }
}

fn client_fixture(fixture: &Fixture) -> ClientIndex {
    let manifest = ClientManifest::from_lanes(
        RemoteGeneration::new(fixture.generation, fixture.snapshot),
        ManifestEpoch::new(nonzero(1)),
        fixture.exact.clone().into_boxed_slice(),
        fixture.lexical.clone().into_boxed_slice(),
    )
    .unwrap_or_else(|_| panic!("benchmark fixture manifest"));
    let mut client = ClientIndex::new();
    client.accept_manifest(
        RemoteManifest::initial(manifest),
        SyncCancellation::Continue,
    );
    for resident in &fixture.resident {
        client
            .record_resident_range(interface_core::ResidentRange::new(
                resident.segment(),
                resident.range(),
            ))
            .unwrap_or_else(|_| panic!("benchmark resident range"));
    }
    client
}

fn baseline(fixture: &Fixture) -> u64 {
    let mut checksum = 0_u64;
    let mut demand_position = 0_u64;
    for demand in &fixture.demands {
        demand_position += 1;
        let range = demand.range();
        let mut matched = false;
        for segment in fixture.exact.iter().chain(&fixture.lexical) {
            if segment.id() == demand.segment()
                && segment.full_range().start() <= range.start()
                && segment.full_range().end() >= range.end()
            {
                matched = true;
            }
        }
        // The historical per-item path checked residence in a second scan after manifest
        // admission. The production selection path retains one validated receipt table.
        let resident = fixture.resident.iter().any(|present| {
            present.segment() == demand.segment()
                && present.range().start() <= range.start()
                && present.range().end() >= range.end()
        });
        let state = if matched && resident { 0xA5 } else { 0x5A };
        checksum = checksum.rotate_left(7)
            ^ range.byte_length()
            ^ demand_position
            ^ segment_checksum(demand.segment())
            ^ state;
    }
    checksum
}

fn production<'index>(
    client: &'index ClientIndex,
    demands: &[SegmentDemand],
    scratch: &mut [Option<LocalSelection<'index>>],
    output: &mut [Option<LocalSelection<'index>>],
) -> u64 {
    let selection = DemandSelection::new(demands)
        .unwrap_or_else(|_| panic!("benchmark demands are strictly ordered"));
    let terminal = client
        .select_local(
            selection,
            SyncCancellation::Continue,
            SelectionScratch::new(scratch),
            SelectionOutput::new(output),
        )
        .unwrap_or_else(|_| panic!("benchmark selection is admitted"));
    let selections = match terminal {
        LocalQueryTerminal::Complete { selections, .. }
        | LocalQueryTerminal::Partial { selections, .. } => selections,
        LocalQueryTerminal::Cancelled => panic!("benchmark fixture cannot be cancelled"),
    };
    let mut checksum = 0_u64;
    let mut selection_position = 0_u64;
    for selection in selections.iter().flatten() {
        selection_position += 1;
        let (range, segment, state) = match selection {
            LocalSelection::Present(selection) => {
                (selection.range(), selection.segment().id(), 0xA5)
            }
            LocalSelection::Missing(resident) => (resident.range(), resident.segment(), 0x5A),
        };
        checksum = checksum.rotate_left(7)
            ^ range.byte_length()
            ^ selection_position
            ^ segment_checksum(segment)
            ^ state;
    }
    checksum
}

fn percentile(samples: &mut [u128; SAMPLES], percentile: usize) -> u128 {
    samples.sort_unstable();
    samples[(SAMPLES * percentile).div_ceil(100).saturating_sub(1)]
}

fn main() {
    let fixture = fixture();
    let expected = baseline(&fixture);
    let client = client_fixture(&fixture);
    let mut scratch = [None; LANE_LEN * DEMANDS_PER_SEGMENT * 2];
    let mut output = [None; LANE_LEN * DEMANDS_PER_SEGMENT * 2];
    let mut baseline_samples = [0_u128; SAMPLES];
    let mut production_samples = [0_u128; SAMPLES];
    for _ in 0..WARMUPS {
        let mut checksum = 0_u64;
        for _ in 0..OPERATIONS_PER_SAMPLE {
            checksum = checksum.rotate_left(1)
                ^ std::hint::black_box(production(
                    &client,
                    &fixture.demands,
                    &mut scratch,
                    &mut output,
                ));
        }
        std::hint::black_box(checksum);
    }
    for (baseline_slot, production_slot) in baseline_samples
        .iter_mut()
        .zip(production_samples.iter_mut())
    {
        let start = Instant::now();
        let mut baseline_checksum = 0_u64;
        for _ in 0..OPERATIONS_PER_SAMPLE {
            baseline_checksum =
                baseline_checksum.rotate_left(1) ^ std::hint::black_box(baseline(&fixture));
        }
        *baseline_slot = start.elapsed().as_nanos() / u128::from(OPERATIONS_PER_SAMPLE);

        let start = Instant::now();
        let mut production_checksum = 0_u64;
        for _ in 0..OPERATIONS_PER_SAMPLE {
            production_checksum = production_checksum.rotate_left(1)
                ^ std::hint::black_box(production(
                    &client,
                    &fixture.demands,
                    &mut scratch,
                    &mut output,
                ));
        }
        *production_slot = start.elapsed().as_nanos() / u128::from(OPERATIONS_PER_SAMPLE);
        assert_eq!(baseline_checksum, production_checksum);
        let mut expected_checksum = 0_u64;
        for _ in 0..OPERATIONS_PER_SAMPLE {
            expected_checksum = expected_checksum.rotate_left(1) ^ expected;
        }
        assert_eq!(expected_checksum, production_checksum);
    }

    let baseline_median = percentile(&mut baseline_samples, 50);
    let baseline_p95 = percentile(&mut baseline_samples, 95);
    let production_median = percentile(&mut production_samples, 50);
    let production_p95 = percentile(&mut production_samples, 95);
    println!(
        "client_sync local-select: baseline median={baseline_median}ns/op p95={baseline_p95}ns/op; production median={production_median}ns/op p95={production_p95}ns/op; samples={SAMPLES} warmups={WARMUPS} operations/sample={OPERATIONS_PER_SAMPLE} checksum={expected:#x}"
    );
}
