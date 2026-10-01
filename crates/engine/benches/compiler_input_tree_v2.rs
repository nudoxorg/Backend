//! Deterministic release measurements for the V2 compiler-input page tree.
//!
//! This compares full ordered-tree rebuilds with same-key immutable path-copy
//! updates and delta transfer. It does not read a real workspace or measure
//! CAS persistence/transport. Independent range-minimum reference trees encode
//! the documented page format, treap priority, version digests, and store
//! object commitment.
//! It does not read the implementation's pages or use its roots to construct
//! expected page identities.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::unwrap_used
)]

use std::{
    collections::{BTreeMap, HashSet},
    env,
    error::Error,
    hint::black_box,
    process::Command,
    sync::Barrier,
    thread,
    time::Instant,
};

use allocation_counter::{AllocationInfo, measure};
use backend_engine::{
    blake3,
    compiler_cluster_transport::{
        CompilerInputMerkleTreeV2, CompilerInputTreeKindV2, CompilerInputTreeRecordV2,
        CompilerInputTreeReplacementV2, CompilerWorkspaceFileRoleV2,
    },
};

const DEFAULT_FILES: usize = 100_000;
const DEFAULT_SAMPLES: usize = 101;
const DEFAULT_WARMUPS: usize = 2;
const SHARD_COUNT: usize = 100;
const MODULE_COUNT: usize = 17;
const STORE_OBJECT_DOMAIN: &[u8] = b"store.object.v1\0";
const VERSION_HASH_DOMAIN: &[u8] = b"backend.version.v2\0";
const PAGE_MAGIC: &[u8; 8] = b"BKCIPG02";
const PRIORITY_DOMAIN: &[u8] = b"backend.compiler.input.path-priority.v2\0";

#[derive(Clone, Copy)]
struct Options {
    files: usize,
    samples: usize,
    warmups: usize,
    readers: [usize; 3],
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ReferenceRecord {
    Directory {
        path: String,
    },
    File {
        path: String,
        role: u8,
        object_id: [u8; 32],
        length: u64,
    },
}

impl ReferenceRecord {
    fn path(&self) -> &str {
        match self {
            Self::Directory { path } | Self::File { path, .. } => path,
        }
    }

    fn tag(&self) -> u8 {
        match self {
            Self::Directory { .. } => 1,
            Self::File { .. } => 2,
        }
    }

    fn key(&self) -> Vec<u8> {
        let mut key = self.path().as_bytes().to_vec();
        key.push(self.tag());
        key
    }

    fn to_implementation_record(&self) -> CompilerInputTreeRecordV2 {
        match self {
            Self::Directory { path } => CompilerInputTreeRecordV2::Directory {
                path: path.clone().into_boxed_str(),
            },
            Self::File {
                path,
                role,
                object_id,
                length,
            } => CompilerInputTreeRecordV2::File {
                path: path.clone().into_boxed_str(),
                role: match role {
                    1 => CompilerWorkspaceFileRoleV2::Source,
                    2 => CompilerWorkspaceFileRoleV2::Configuration,
                    3 => CompilerWorkspaceFileRoleV2::Lock,
                    _ => CompilerWorkspaceFileRoleV2::Other,
                },
                object_id: *object_id,
                length: *length,
            },
        }
    }
}

struct Fixture {
    records: Vec<ReferenceRecord>,
    edit_records: Vec<ReferenceRecord>,
    edit_path: String,
    source_bytes: u64,
    changed_file_length_delta: u64,
}

#[derive(Clone, Copy)]
struct BuildSample {
    elapsed_ns: u128,
    allocation_count: u64,
    allocated_bytes: u64,
    max_live_bytes_allocated_inside_closure: u64,
}

struct OracleNode {
    id: [u8; 32],
    encoded_bytes: usize,
    left: Option<Box<Self>>,
    right: Option<Box<Self>>,
}

struct ReferenceTree {
    root: Option<Box<OracleNode>>,
    root_id: [u8; 32],
    ids: HashSet<[u8; 32]>,
    page_count: usize,
    encoded_bytes: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_options()?;
    verify_independent_small_oracle()?;

    let fixture = large_fixture(options.files)?;
    let expected_changed_paths =
        independently_changed_file_paths(&fixture.records, &fixture.edit_records);
    if expected_changed_paths.len() != 1 || expected_changed_paths[0] != fixture.edit_path {
        return Err(
            "independent workspace oracle did not find exactly the selected one-file edit".into(),
        );
    }
    let directory_count = fixture
        .records
        .iter()
        .filter(|record| matches!(record, ReferenceRecord::Directory { .. }))
        .count();
    let record_count = fixture.records.len();
    println!(
        "compiler_input_tree_v2 protocol=2 profile=release fixture=synthetic_metadata_only files={} directories={} records={} samples={} warmups={} readers=1,4,16 seed=0x5eed_cafe_d00d_beef source_bytes_are_declared_lengths=true physical_files_read=false allocation_scope=allocation_counter_measured_closure_current_thread preexisting_input_allocations_excluded=true rss_samples=current_process_snapshots peak_rss_requires_time_l",
        options.files, directory_count, record_count, options.samples, options.warmups
    );

    let (base, baseline_samples) = measure_builds(&fixture.records, options)?;
    report_build("baseline_full_tree", &baseline_samples, &base);
    print_process_rss("after_baseline_tree");

    let noop_samples = measure_delta_samples(&base, &base, options.samples)?;
    let noop = base.delta_from(&base)?;
    if !noop.page_ids.is_empty() || noop.transfer_bytes != 0 || noop.visited_pages != 0 {
        return Err("independent no-op invariant failed".into());
    }
    report_delta("warm_noop_delta", &noop_samples, &noop);

    let replacement = fixture
        .edit_records
        .iter()
        .find(|record| record.path() == fixture.edit_path)
        .ok_or("independent edit record was missing")?
        .to_implementation_record();
    let (edited, edit_build_samples) = measure_builds(&fixture.edit_records, options)?;
    report_build("one_edit_full_rebuild", &edit_build_samples, &edited);
    let (updated, update_samples, update_stats) = measure_updates(&base, &replacement, options)?;
    report_build("one_edit_update_construction", &update_samples, &updated);
    if updated.root() != edited.root() {
        return Err("persistent update root disagrees with the full rebuild".into());
    }
    print_process_rss("with_base_full_rebuild_and_persistent_update");

    let one_edit_samples = measure_delta_samples(&updated, &base, options.samples)?;
    let one_edit = updated.delta_from(&base)?;
    let rebuilt_delta = edited.delta_from(&base)?;
    if one_edit != rebuilt_delta {
        return Err("persistent update delta disagrees with the full rebuild delta".into());
    }
    if one_edit.page_ids.is_empty()
        || one_edit.page_ids.len() >= edited.page_count()
        || one_edit.transfer_bytes >= edited.encoded_bytes()
    {
        return Err("one-edit delta did not share any unchanged pages".into());
    }
    report_delta("one_edit_persistent_delta", &one_edit_samples, &one_edit);
    println!(
        "compiler_input_tree_v2 input_work files={} expected_changed_files=1 edit_path={} declared_file_bytes={} changed_file_length_delta_bytes={} base_pages={} edited_pages={} base_page_bytes={} edited_page_bytes={} path_copied_pages={} inherited_pages={}",
        options.files,
        fixture.edit_path,
        fixture.source_bytes,
        fixture.changed_file_length_delta,
        base.page_count(),
        edited.page_count(),
        base.encoded_bytes(),
        edited.encoded_bytes(),
        update_stats.path_copied_pages,
        updated
            .page_count()
            .saturating_sub(update_stats.path_copied_pages)
    );

    verify_independent_large_oracle(&fixture, &base, &updated, &edited)?;

    for readers in options.readers {
        let (noop_batch, noop_readers) =
            run_parallel_delta(&base, &base, readers, options.samples)?;
        report_parallel("warm_noop_delta", readers, &noop_batch, &noop_readers);
        let (edit_batch, edit_readers) =
            run_parallel_delta(&updated, &base, readers, options.samples)?;
        report_parallel("one_edit_delta", readers, &edit_batch, &edit_readers);
    }

    drop(edited);
    drop(updated);
    drop(base);
    print_process_rss("after_tree_drop");
    println!(
        "compiler_input_tree_v2 limitations capture=not_measured cas=not_measured transport=not_measured peak_rss=external_wrapper_required filesystem_page_cache=not_applicable"
    );
    Ok(())
}

fn parse_options() -> Result<Options, Box<dyn Error>> {
    let mut options = Options {
        files: DEFAULT_FILES,
        samples: DEFAULT_SAMPLES,
        warmups: DEFAULT_WARMUPS,
        readers: [1, 4, 16],
    };
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        // Cargo appends this libtest flag to `cargo bench` targets even when
        // the target uses a custom harness. It has no meaning for our runner.
        if flag == "--bench" {
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--files" => options.files = value.parse()?,
            "--samples" => options.samples = value.parse()?,
            "--warmups" => options.warmups = value.parse()?,
            "--readers" => {
                let values = value
                    .split(',')
                    .map(str::parse::<usize>)
                    .collect::<Result<Vec<_>, _>>()?;
                if values.as_slice() != [1, 4, 16] {
                    return Err("--readers is fixed to 1,4,16 for comparable runs".into());
                }
            }
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    if options.files == 0 || options.files > 140_000 {
        return Err(
            "--files must be between 1 and 140000 under the V2 record-admission charge".into(),
        );
    }
    if options.samples < 100 || options.samples > 501 {
        return Err("--samples must be between 100 and 501 for p99 reporting".into());
    }
    if options.warmups > 20 {
        return Err("--warmups must be at most 20".into());
    }
    Ok(options)
}

fn independently_changed_file_paths(
    old: &[ReferenceRecord],
    new: &[ReferenceRecord],
) -> Vec<String> {
    let old_files: BTreeMap<_, _> = old
        .iter()
        .filter_map(|record| match record {
            ReferenceRecord::File {
                path,
                role,
                object_id,
                length,
            } => Some((path.as_str(), (*role, *object_id, *length))),
            ReferenceRecord::Directory { .. } => None,
        })
        .collect();
    let new_files: BTreeMap<_, _> = new
        .iter()
        .filter_map(|record| match record {
            ReferenceRecord::File {
                path,
                role,
                object_id,
                length,
            } => Some((path.as_str(), (*role, *object_id, *length))),
            ReferenceRecord::Directory { .. } => None,
        })
        .collect();
    old_files
        .keys()
        .chain(new_files.keys())
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|path| old_files.get(path) != new_files.get(path))
        .map(str::to_owned)
        .collect()
}

fn large_fixture(files: usize) -> Result<Fixture, Box<dyn Error>> {
    let mut records = Vec::with_capacity(files + SHARD_COUNT * (MODULE_COUNT + 1) + 1);
    records.push(ReferenceRecord::Directory {
        path: String::new(),
    });
    for shard in 0..SHARD_COUNT {
        records.push(ReferenceRecord::Directory {
            path: format!("shard-{shard:03}"),
        });
        for module in 0..MODULE_COUNT {
            records.push(ReferenceRecord::Directory {
                path: format!("shard-{shard:03}/mod-{module:02}"),
            });
        }
    }

    let mut source_bytes = 0_u64;
    let mut edit_path = String::new();
    let edit_index = files.saturating_mul(37) / 101;
    for index in 0..files {
        let shard = index % SHARD_COUNT;
        let module = index.wrapping_mul(7).wrapping_add(3) % MODULE_COUNT;
        let suffix = match index % 7 {
            0 => "alpha",
            1 => "zeta",
            2 => "cfg",
            3 => "generated",
            4 => "test_support",
            5 => "impl_detail",
            _ => "omega",
        };
        let path = format!("shard-{shard:03}/mod-{module:02}/unit-{index:06}_{suffix}.rs");
        let object_id = fixture_object_id(index, &path, 0);
        let length = 31_u64 + u64::try_from(index.wrapping_mul(7_919).wrapping_add(137) % 16_384)?;
        source_bytes = source_bytes.saturating_add(length);
        if index == edit_index {
            edit_path.clone_from(&path);
        }
        records.push(ReferenceRecord::File {
            path,
            role: match index % 19 {
                0 => 2,
                1 => 3,
                2 => 4,
                _ => 1,
            },
            object_id,
            length,
        });
    }
    records.sort_by_key(ReferenceRecord::key);

    let mut edit_records = records.clone();
    let edit_record = edit_records
        .iter_mut()
        .find(|record| record.path() == edit_path)
        .ok_or("independent fixture edit path was missing")?;
    let ReferenceRecord::File {
        object_id, length, ..
    } = edit_record
    else {
        return Err("independent fixture edit path did not name a file".into());
    };
    let old_length = *length;
    *object_id = fixture_object_id(edit_index, &edit_path, 1);
    *length = old_length.saturating_add(29);
    edit_records.sort_by_key(ReferenceRecord::key);
    Ok(Fixture {
        records,
        edit_records,
        edit_path,
        source_bytes,
        changed_file_length_delta: 29,
    })
}

fn fixture_object_id(index: usize, path: &str, revision: u8) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.compiler-input-tree-benchmark.object.v1\0");
    hasher.update(&revision.to_be_bytes());
    hasher.update(&u64::try_from(index).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(&u64::try_from(path.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(path.as_bytes());
    *hasher.finalize().as_bytes()
}

fn implementation_records(records: &[ReferenceRecord]) -> Vec<CompilerInputTreeRecordV2> {
    records
        .iter()
        .map(ReferenceRecord::to_implementation_record)
        .collect()
}

fn build_implementation_tree(
    records: &[ReferenceRecord],
) -> Result<CompilerInputMerkleTreeV2, Box<dyn Error>> {
    Ok(CompilerInputMerkleTreeV2::from_sorted_records(
        CompilerInputTreeKindV2::Workspace,
        implementation_records(records),
    )?)
}

fn measure_builds(
    records: &[ReferenceRecord],
    options: Options,
) -> Result<(CompilerInputMerkleTreeV2, Vec<BuildSample>), Box<dyn Error>> {
    for _ in 0..options.warmups {
        drop(build_implementation_tree(records)?);
    }

    let mut samples = Vec::with_capacity(options.samples);
    let mut retained = None;
    for index in 0..options.samples {
        // Clone and convert the fixed source facts outside the timed region.
        let input = implementation_records(records);
        let mut built = None;
        let start = Instant::now();
        let allocation = measure(|| {
            built = Some(CompilerInputMerkleTreeV2::from_sorted_records(
                CompilerInputTreeKindV2::Workspace,
                input,
            ));
        });
        let elapsed_ns = start.elapsed().as_nanos();
        let tree = built.ok_or("tree constructor was not measured")??;
        black_box(tree.root());
        samples.push(build_sample(elapsed_ns, allocation));
        if index + 1 == options.samples {
            retained = Some(tree);
        }
    }
    let tree = retained.ok_or("baseline tree sample was not retained")?;
    Ok((tree, samples))
}

fn measure_updates(
    base: &CompilerInputMerkleTreeV2,
    replacement: &CompilerInputTreeRecordV2,
    options: Options,
) -> Result<
    (
        CompilerInputMerkleTreeV2,
        Vec<BuildSample>,
        backend_engine::compiler_cluster_transport::CompilerInputTreeUpdateStatsV2,
    ),
    Box<dyn Error>,
> {
    for _ in 0..options.warmups {
        drop(
            base.apply_replacements(vec![CompilerInputTreeReplacementV2::new(
                replacement.clone(),
            )])?,
        );
    }

    let mut samples = Vec::with_capacity(options.samples);
    let mut retained = None;
    for index in 0..options.samples {
        // The replacement object is input setup, allocated before measurement.
        let input = vec![CompilerInputTreeReplacementV2::new(replacement.clone())];
        let mut built = None;
        let start = Instant::now();
        let allocation = measure(|| {
            built = Some(base.apply_replacements_with_stats(input));
        });
        let elapsed_ns = start.elapsed().as_nanos();
        let (tree, stats) = built.ok_or("persistent update was not measured")??;
        black_box(tree.root());
        samples.push(build_sample(elapsed_ns, allocation));
        if index + 1 == options.samples {
            retained = Some((tree, stats));
        }
    }
    let (tree, stats) = retained.ok_or("persistent update sample was not retained")?;
    Ok((tree, samples, stats))
}

fn measure_delta_samples(
    current: &CompilerInputMerkleTreeV2,
    base: &CompilerInputMerkleTreeV2,
    samples: usize,
) -> Result<Vec<BuildSample>, Box<dyn Error>> {
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut observed = None;
        let start = Instant::now();
        let allocation = measure(|| observed = Some(current.delta_from(base)));
        let elapsed_ns = start.elapsed().as_nanos();
        let delta = observed.ok_or("delta operation was not measured")??;
        black_box(delta.page_ids.len());
        timings.push(build_sample(elapsed_ns, allocation));
    }
    Ok(timings)
}

fn build_sample(elapsed_ns: u128, allocation: AllocationInfo) -> BuildSample {
    BuildSample {
        elapsed_ns,
        allocation_count: allocation.count_total,
        allocated_bytes: allocation.bytes_total,
        max_live_bytes_allocated_inside_closure: allocation.bytes_max,
    }
}

fn report_build(scenario: &str, samples: &[BuildSample], tree: &CompilerInputMerkleTreeV2) {
    let elapsed: Vec<_> = samples.iter().map(|sample| sample.elapsed_ns).collect();
    let counts: Vec<_> = samples
        .iter()
        .map(|sample| u128::from(sample.allocation_count))
        .collect();
    let allocated: Vec<_> = samples
        .iter()
        .map(|sample| u128::from(sample.allocated_bytes))
        .collect();
    let peak_live: Vec<_> = samples
        .iter()
        .map(|sample| u128::from(sample.max_live_bytes_allocated_inside_closure))
        .collect();
    println!(
        "compiler_input_tree_v2 scenario={scenario} samples={} p50_ns={} p95_ns={} p99_ns={} page_count={} encoded_page_bytes={} allocations_p50={} allocated_bytes_p50={} max_live_bytes_allocated_inside_closure_p50={} root={}",
        samples.len(),
        percentile(&elapsed, 50),
        percentile(&elapsed, 95),
        percentile(&elapsed, 99),
        tree.page_count(),
        tree.encoded_bytes(),
        percentile(&counts, 50),
        percentile(&allocated, 50),
        percentile(&peak_live, 50),
        hex(&tree.root())
    );
}

fn report_delta(
    scenario: &str,
    samples: &[BuildSample],
    delta: &backend_engine::compiler_cluster_transport::CompilerInputPageDeltaV2,
) {
    let elapsed: Vec<_> = samples.iter().map(|sample| sample.elapsed_ns).collect();
    let counts: Vec<_> = samples
        .iter()
        .map(|sample| u128::from(sample.allocation_count))
        .collect();
    let allocated: Vec<_> = samples
        .iter()
        .map(|sample| u128::from(sample.allocated_bytes))
        .collect();
    println!(
        "compiler_input_tree_v2 scenario={scenario} samples={} p50_ns={} p95_ns={} p99_ns={} delta_pages={} transfer_bytes={} visited_pages={} allocations_p50={} allocated_bytes_p50={}",
        samples.len(),
        percentile(&elapsed, 50),
        percentile(&elapsed, 95),
        percentile(&elapsed, 99),
        delta.page_ids.len(),
        delta.transfer_bytes,
        delta.visited_pages,
        percentile(&counts, 50),
        percentile(&allocated, 50)
    );
}

fn run_parallel_delta(
    current: &CompilerInputMerkleTreeV2,
    base: &CompilerInputMerkleTreeV2,
    readers: usize,
    samples: usize,
) -> Result<(Vec<u128>, Vec<u128>), Box<dyn Error>> {
    let start_barrier = Barrier::new(readers + 1);
    let finish_barrier = Barrier::new(readers + 1);
    let expected_page_count = current.delta_from(base)?.page_ids.len();
    thread::scope(|scope| {
        let mut workers = Vec::with_capacity(readers);
        for _ in 0..readers {
            let current = current;
            let base = base;
            let start = &start_barrier;
            let finish = &finish_barrier;
            workers.push(scope.spawn(move || {
                let mut elapsed = Vec::with_capacity(samples);
                let mut all_valid = true;
                for _ in 0..samples {
                    start.wait();
                    let operation = Instant::now();
                    let delta = current.delta_from(base);
                    let elapsed_ns = operation.elapsed().as_nanos();
                    let valid = delta
                        .as_ref()
                        .is_ok_and(|observed| observed.page_ids.len() == expected_page_count);
                    black_box(delta.as_ref().ok().map(|value| value.transfer_bytes));
                    elapsed.push(elapsed_ns);
                    all_valid &= valid;
                    drop(delta);
                    finish.wait();
                }
                if all_valid {
                    Ok(elapsed)
                } else {
                    Err(std::io::Error::other("parallel delta result diverged"))
                }
            }));
        }

        let mut batch_elapsed = Vec::with_capacity(samples);
        for _ in 0..samples {
            let batch = Instant::now();
            start_barrier.wait();
            finish_barrier.wait();
            batch_elapsed.push(batch.elapsed().as_nanos());
        }
        let mut reader_elapsed = Vec::with_capacity(readers.saturating_mul(samples));
        for worker in workers {
            let values = worker
                .join()
                .map_err(|_| std::io::Error::other("parallel benchmark worker panicked"))??;
            reader_elapsed.extend(values);
        }
        Ok::<_, Box<dyn Error>>((batch_elapsed, reader_elapsed))
    })
}

fn report_parallel(scenario: &str, readers: usize, batch: &[u128], per_reader: &[u128]) {
    println!(
        "compiler_input_tree_v2 scenario=parallel_{scenario} readers={readers} samples={} batch_complete_p50_ns={} batch_complete_p95_ns={} batch_complete_p99_ns={} reader_call_p50_ns={} reader_call_p95_ns={} reader_call_p99_ns={} operations={}",
        batch.len(),
        percentile(batch, 50),
        percentile(batch, 95),
        percentile(batch, 99),
        percentile(per_reader, 50),
        percentile(per_reader, 95),
        percentile(per_reader, 99),
        per_reader.len()
    );
}

fn verify_independent_small_oracle() -> Result<(), Box<dyn Error>> {
    let old_records = small_asymmetric_records(false);
    let new_records = small_asymmetric_records(true);
    let expected_old = ReferenceTree::from_records(&old_records)?;
    let expected_new = ReferenceTree::from_records(&new_records)?;

    let actual_old = build_implementation_tree(&old_records)?;
    let actual_new = build_implementation_tree(&new_records)?;
    if actual_old.root() != expected_old.root_id || actual_new.root() != expected_new.root_id {
        return Err("V2 root differs from the independent small reference tree".into());
    }
    let actual_old_ids: HashSet<_> = actual_old.pages().map(|page| page.id()).collect();
    let actual_new_ids: HashSet<_> = actual_new.pages().map(|page| page.id()).collect();
    if actual_old_ids != expected_old.ids || actual_new_ids != expected_new.ids {
        return Err("V2 page IDs differ from the independent small reference tree".into());
    }

    let expected_delta = expected_new.delta_from(&expected_old);
    let actual_delta = actual_new.delta_from(&actual_old)?;
    if !delta_matches_reference(
        &expected_delta,
        &actual_delta.page_ids,
        actual_delta.transfer_bytes,
    ) || actual_delta.visited_pages != expected_delta.page_ids.len()
        || actual_old.page_count() != expected_old.page_count
        || actual_new.page_count() != expected_new.page_count
        || actual_old.encoded_bytes() != expected_old.encoded_bytes
        || actual_new.encoded_bytes() != expected_new.encoded_bytes
    {
        return Err("V2 changed page IDs differ from the independent small reference delta".into());
    }

    // Deliberately falsify the claimed transfer twice. Both mutations must be
    // rejected by the same fixed oracle that accepts the real delta.
    let empty_delta_mutation: Vec<[u8; 32]> = Vec::new();
    if delta_matches_reference(&expected_delta, &empty_delta_mutation, 0) {
        return Err("independent oracle accepted an empty-delta mutation".into());
    }
    let (full_page_ids, full_page_bytes) = expected_new.all_pages();
    if delta_matches_reference(&expected_delta, &full_page_ids, full_page_bytes) {
        return Err("independent oracle accepted a full-tree-transfer mutation".into());
    }

    // Simulate a lost edit. The independent reference says the expected head
    // moved and names concrete changed pages, so a stale-base implementation
    // cannot pass by deriving its expected value from the implementation.
    let skipped_edit = build_implementation_tree(&old_records)?;
    if skipped_edit.root() == expected_new.root_id || expected_delta.page_ids.is_empty() {
        return Err("independent edit oracle failed to reject a skipped edit".into());
    }
    println!(
        "compiler_input_tree_v2 oracle=independent_small_reference records={} old_pages={} new_pages={} old_page_bytes={} new_page_bytes={} expected_changed_page_ids={} expected_transfer_bytes={} mutation_empty_delta=rejected mutation_full_tree_transfer=rejected mutation_skip_edit=rejected old_root={} new_root={}",
        old_records.len(),
        expected_old.page_count,
        expected_new.page_count,
        expected_old.encoded_bytes,
        expected_new.encoded_bytes,
        expected_delta.page_ids.len(),
        expected_delta.transfer_bytes,
        hex(&expected_old.root_id),
        hex(&expected_new.root_id)
    );
    Ok(())
}

fn verify_independent_large_oracle(
    fixture: &Fixture,
    base: &CompilerInputMerkleTreeV2,
    updated: &CompilerInputMerkleTreeV2,
    rebuilt: &CompilerInputMerkleTreeV2,
) -> Result<(), Box<dyn Error>> {
    let expected_base = ReferenceTree::from_records(&fixture.records)?;
    let expected_updated = ReferenceTree::from_records(&fixture.edit_records)?;
    let base_ids: HashSet<_> = base.pages().map(|page| page.id()).collect();
    let updated_ids: HashSet<_> = updated.pages().map(|page| page.id()).collect();
    let rebuilt_ids: HashSet<_> = rebuilt.pages().map(|page| page.id()).collect();
    if base.root() != expected_base.root_id
        || updated.root() != expected_updated.root_id
        || rebuilt.root() != expected_updated.root_id
        || base_ids != expected_base.ids
        || updated_ids != expected_updated.ids
        || rebuilt_ids != expected_updated.ids
        || base.page_count() != expected_base.page_count
        || updated.page_count() != expected_updated.page_count
        || updated.encoded_bytes() != expected_updated.encoded_bytes
    {
        return Err("100k tree differs from the independent workspace oracle".into());
    }
    let expected_delta = expected_updated.delta_from(&expected_base);
    let observed_delta = updated.delta_from(base)?;
    if !delta_matches_reference(
        &expected_delta,
        &observed_delta.page_ids,
        observed_delta.transfer_bytes,
    ) || observed_delta.visited_pages != expected_delta.page_ids.len()
    {
        return Err("100k persistent update delta differs from the independent oracle".into());
    }
    println!(
        "compiler_input_tree_v2 oracle=independent_100k_cartesian_reference files={} records={} base_pages={} updated_pages={} expected_changed_page_ids={} expected_transfer_bytes={} full_rebuild_root={} persistent_update_root={}",
        fixture
            .records
            .iter()
            .filter(|record| matches!(record, ReferenceRecord::File { .. }))
            .count(),
        fixture.records.len(),
        expected_base.page_count,
        expected_updated.page_count,
        expected_delta.page_ids.len(),
        expected_delta.transfer_bytes,
        hex(&expected_updated.root_id),
        hex(&updated.root())
    );
    Ok(())
}

fn small_asymmetric_records(edited: bool) -> Vec<ReferenceRecord> {
    let mut records = vec![
        ReferenceRecord::Directory {
            path: String::new(),
        },
        ReferenceRecord::Directory {
            path: "alpha".to_owned(),
        },
        ReferenceRecord::Directory {
            path: "alpha/data".to_owned(),
        },
        ReferenceRecord::Directory {
            path: "m".to_owned(),
        },
        ReferenceRecord::Directory {
            path: "m/src".to_owned(),
        },
        ReferenceRecord::Directory {
            path: "z".to_owned(),
        },
        ReferenceRecord::Directory {
            path: "z/a".to_owned(),
        },
        ReferenceRecord::File {
            path: "alpha/data/left.rs".to_owned(),
            role: 1,
            object_id: [0x13; 32],
            length: 9,
        },
        ReferenceRecord::File {
            path: "m/Build.lock".to_owned(),
            role: 3,
            object_id: [0x27; 32],
            length: 1_024,
        },
        ReferenceRecord::File {
            path: "m/src/README.md".to_owned(),
            role: 4,
            object_id: if edited { [0xa4; 32] } else { [0xa3; 32] },
            length: if edited { 62 } else { 33 },
        },
        ReferenceRecord::File {
            path: "z/a/omega.rs".to_owned(),
            role: 1,
            object_id: [0xc1; 32],
            length: 5,
        },
        ReferenceRecord::File {
            path: "z/tail.bin".to_owned(),
            role: 2,
            object_id: [0x6b; 32],
            length: 4_097,
        },
    ];
    records.sort_by_key(ReferenceRecord::key);
    records
}

impl ReferenceTree {
    fn from_records(records: &[ReferenceRecord]) -> Result<Self, Box<dyn Error>> {
        let mut ordered = records.to_vec();
        ordered.sort_by_key(ReferenceRecord::key);
        if ordered.is_empty() {
            let root_id = reference_empty_root(1);
            return Ok(Self {
                root: None,
                root_id,
                ids: HashSet::new(),
                page_count: 0,
                encoded_bytes: 0,
            });
        }
        let mut priorities = Vec::with_capacity(ordered.len());
        let mut unique_priorities = HashSet::with_capacity(ordered.len());
        for record in &ordered {
            let priority = reference_priority(record);
            if !unique_priorities.insert(priority) {
                return Err("independent reference priority collision".into());
            }
            priorities.push(priority);
        }
        let leaf_count = ordered
            .len()
            .checked_next_power_of_two()
            .ok_or("reference range-index size overflow")?;
        let mut range_min = vec![usize::MAX; leaf_count.saturating_mul(2)];
        for index in 0..ordered.len() {
            range_min[leaf_count + index] = index;
        }
        for index in (1..leaf_count).rev() {
            range_min[index] =
                reference_choose_min(range_min[index * 2], range_min[index * 2 + 1], &priorities);
        }
        let root = reference_build_range(
            &ordered,
            &priorities,
            &range_min,
            leaf_count,
            0,
            ordered.len(),
            1,
        )?;
        let root_id = root
            .as_ref()
            .map_or_else(|| reference_empty_root(1), |node| node.id);
        let mut ids = HashSet::with_capacity(ordered.len());
        let mut page_count = 0;
        let mut encoded_bytes = 0;
        collect_reference_ids(&root, &mut ids, &mut page_count, &mut encoded_bytes);
        Ok(Self {
            root,
            root_id,
            ids,
            page_count,
            encoded_bytes,
        })
    }

    fn delta_from(&self, base: &Self) -> ReferenceDelta {
        let mut page_ids = Vec::new();
        let mut transfer_bytes = 0;
        collect_reference_delta(&self.root, &base.ids, &mut page_ids, &mut transfer_bytes);
        ReferenceDelta {
            page_ids,
            transfer_bytes,
        }
    }

    fn all_pages(&self) -> (Vec<[u8; 32]>, usize) {
        let mut page_ids = Vec::with_capacity(self.page_count);
        let mut transfer_bytes = 0;
        collect_all_reference_pages(&self.root, &mut page_ids, &mut transfer_bytes);
        (page_ids, transfer_bytes)
    }
}

struct ReferenceDelta {
    page_ids: Vec<[u8; 32]>,
    transfer_bytes: usize,
}

fn delta_matches_reference(
    expected: &ReferenceDelta,
    observed_page_ids: &[[u8; 32]],
    observed_transfer_bytes: usize,
) -> bool {
    expected.page_ids == observed_page_ids && expected.transfer_bytes == observed_transfer_bytes
}

fn reference_priority(record: &ReferenceRecord) -> [u8; 32] {
    let path = record.path().as_bytes();
    let mut hasher = blake3::Hasher::new();
    hasher.update(PRIORITY_DOMAIN);
    hasher.update(&[1]); // Workspace tree kind.
    hasher.update(&(u32::try_from(path.len()).unwrap_or(u32::MAX) + 1).to_be_bytes());
    hasher.update(path);
    hasher.update(&[record.tag()]);
    *hasher.finalize().as_bytes()
}

fn reference_choose_min(left: usize, right: usize, priorities: &[[u8; 32]]) -> usize {
    match (left, right) {
        (usize::MAX, candidate) | (candidate, usize::MAX) => candidate,
        (left, right) if priorities[left] <= priorities[right] => left,
        (_, right) => right,
    }
}

fn reference_range_min(
    start: usize,
    end: usize,
    leaf_count: usize,
    range_min: &[usize],
    priorities: &[[u8; 32]],
) -> Option<usize> {
    if start >= end {
        return None;
    }
    let mut left = start + leaf_count;
    let mut right = end + leaf_count;
    let mut best = usize::MAX;
    while left < right {
        if left & 1 == 1 {
            best = reference_choose_min(best, range_min[left], priorities);
            left += 1;
        }
        if right & 1 == 1 {
            right -= 1;
            best = reference_choose_min(best, range_min[right], priorities);
        }
        left /= 2;
        right /= 2;
    }
    (best != usize::MAX).then_some(best)
}

fn reference_build_range(
    records: &[ReferenceRecord],
    priorities: &[[u8; 32]],
    range_min: &[usize],
    leaf_count: usize,
    start: usize,
    end: usize,
    depth: usize,
) -> Result<Option<Box<OracleNode>>, Box<dyn Error>> {
    if start >= end {
        return Ok(None);
    }
    if depth > 512 {
        return Err("independent reference treap exceeded its depth bound".into());
    }
    let root_index = reference_range_min(start, end, leaf_count, range_min, priorities)
        .ok_or("independent range-minimum query returned no root")?;
    let left = reference_build_range(
        records,
        priorities,
        range_min,
        leaf_count,
        start,
        root_index,
        depth + 1,
    )?;
    let right = reference_build_range(
        records,
        priorities,
        range_min,
        leaf_count,
        root_index + 1,
        end,
        depth + 1,
    )?;
    let encoded = reference_encode_page(
        &records[root_index],
        left.as_ref().map(|node| node.id),
        right.as_ref().map(|node| node.id),
    );
    Ok(Some(Box::new(OracleNode {
        id: reference_store_object_id(&encoded),
        encoded_bytes: encoded.len(),
        left,
        right,
    })))
}

fn reference_encode_page(
    record: &ReferenceRecord,
    left: Option<[u8; 32]>,
    right: Option<[u8; 32]>,
) -> Vec<u8> {
    let path = record.path().as_bytes();
    let mut output = Vec::with_capacity(path.len() + 96);
    output.extend_from_slice(PAGE_MAGIC);
    output.extend_from_slice(&[1, record.tag()]); // Workspace kind, record tag.
    output.extend_from_slice(&u32::try_from(path.len()).unwrap_or(u32::MAX).to_be_bytes());
    output.extend_from_slice(path);
    if let ReferenceRecord::File {
        role,
        object_id,
        length,
        ..
    } = record
    {
        output.push(*role);
        output.extend_from_slice(object_id);
        output.extend_from_slice(&length.to_be_bytes());
    }
    reference_encode_child(&mut output, left);
    reference_encode_child(&mut output, right);
    output
}

fn reference_encode_child(output: &mut Vec<u8>, child: Option<[u8; 32]>) {
    match child {
        Some(id) => {
            output.push(1);
            output.extend_from_slice(&id);
        }
        None => output.push(0),
    }
}

fn reference_store_object_id(bytes: &[u8]) -> [u8; 32] {
    let key = reference_version_digest(0x4b, bytes);
    let version = reference_version_digest(0x56, bytes);
    let object_length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let fixed_preimage_length = 1_u64 + 2 + 1 + 64 + 8;
    let preimage_length = fixed_preimage_length.saturating_add(object_length);
    let mut hasher = blake3::Hasher::new();
    hasher.update(STORE_OBJECT_DOMAIN);
    hasher.update(&preimage_length.to_le_bytes());
    hasher.update(&[0xe7]);
    hasher.update(&2_u16.to_le_bytes());
    hasher.update(&[2]);
    hasher.update(&key);
    hasher.update(&version);
    hasher.update(&object_length.to_le_bytes());
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn reference_version_digest(class: u8, bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(VERSION_HASH_DOMAIN);
    hasher.update(&[class, 0xe7]);
    hasher.update(&2_u16.to_be_bytes());
    hasher.update(&[2]);
    hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn reference_empty_root(kind: u8) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.compiler.input.empty-tree.v2\0");
    hasher.update(&[kind]);
    *hasher.finalize().as_bytes()
}

fn collect_reference_ids(
    node: &Option<Box<OracleNode>>,
    ids: &mut HashSet<[u8; 32]>,
    page_count: &mut usize,
    encoded_bytes: &mut usize,
) {
    if let Some(node) = node {
        ids.insert(node.id);
        *page_count += 1;
        *encoded_bytes += node.encoded_bytes;
        collect_reference_ids(&node.left, ids, page_count, encoded_bytes);
        collect_reference_ids(&node.right, ids, page_count, encoded_bytes);
    }
}

fn collect_reference_delta(
    node: &Option<Box<OracleNode>>,
    base_ids: &HashSet<[u8; 32]>,
    page_ids: &mut Vec<[u8; 32]>,
    transfer_bytes: &mut usize,
) {
    if let Some(node) = node {
        if !base_ids.contains(&node.id) {
            page_ids.push(node.id);
            *transfer_bytes += node.encoded_bytes;
            collect_reference_delta(&node.left, base_ids, page_ids, transfer_bytes);
            collect_reference_delta(&node.right, base_ids, page_ids, transfer_bytes);
        }
    }
}

fn collect_all_reference_pages(
    node: &Option<Box<OracleNode>>,
    page_ids: &mut Vec<[u8; 32]>,
    transfer_bytes: &mut usize,
) {
    if let Some(node) = node {
        page_ids.push(node.id);
        *transfer_bytes += node.encoded_bytes;
        collect_all_reference_pages(&node.left, page_ids, transfer_bytes);
        collect_all_reference_pages(&node.right, page_ids, transfer_bytes);
    }
}

fn percentile(values: &[u128], percentile: usize) -> u128 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = (percentile.saturating_mul(sorted.len()).saturating_add(99) / 100).max(1) - 1;
    sorted[rank.min(sorted.len().saturating_sub(1))]
}

fn print_process_rss(stage: &str) {
    let pid = std::process::id().to_string();
    let value = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.split_whitespace().next()?.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1024));
    println!(
        "compiler_input_tree_v2 rss_sample stage={stage} measurement=ps_current_snapshot current_rss_bytes={}",
        value.map_or_else(|| "unavailable".to_owned(), |bytes| bytes.to_string())
    );
}

fn hex(bytes: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}
