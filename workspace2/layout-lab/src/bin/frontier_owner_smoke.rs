//! Safe, bounded owner-layout smoke test; not a production representation.

mod smoke {
    use allocation_counter::{AllocationInfo, measure};
    use bumpalo::{Bump, collections::Vec as BumpVec};
    use core::{
        hint::black_box,
        mem::{MaybeUninit, size_of},
    };
    use nudox_id::{ContentId, ObjectDomain};
    use nudox_object::{ObjectKind, ObjectLength, ObjectRef};
    use nudox_root::{EntryKey, GenerationRoot, RootBuildError, RootEntry};
    use nudox_schema::SchemaId;
    use nudox_store_memory::{InlineMemoryStore, MemoryStore, StoreCapacity};
    use std::process::{Command, ExitCode};
    use thiserror::Error;
    use triomphe::{Arc as TriArc, HeaderSlice, ThinArc};

    type Row = [u64; 8];
    type StoreRow = [u64; 8];
    type ThinRows = ThinArc<[u64; 6], Row>;
    type HeaderRows = TriArc<HeaderSlice<[u64; 6], [Row]>>;

    #[derive(Debug, Error)]
    pub enum RunError {
        #[error("root smoke construction failed")]
        Root(#[from] RootBuildError),
    }

    pub fn main() -> ExitCode {
        match run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("frontier_owner_smoke: {error}");
                ExitCode::FAILURE
            }
        }
    }
    pub fn run() -> Result<(), RunError> {
        println!("format\tfrontier-owner-smoke-v2");
        println!(
            "target\t{}-{}-{}",
            std::env::consts::ARCH,
            std::env::consts::OS,
            usize::BITS
        );
        println!("compiler\t{}", rustc_version());
        println!(
            "profile\t{}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        println!(
            "rustflags\t{}",
            std::env::var("RUSTFLAGS").unwrap_or_else(|_| "none".into())
        );
        println!(
            "columns\tkind\tfamily\tconstruction_scope\tn\tstate\towner_bytes\tvalue_bytes\tretained_backing_bytes\tconstruction_peak_bytes\tpointer_depth\tsequential_reads\trandom_reads\tchecksum\talloc_total\talloc_net_after_drop\talloc_max\tbytes_total\tbytes_net_after_drop\tbytes_max"
        );
        for n in [0, 1, 4, 5, 1024] {
            root_rows(n)?;
            store_rows(n);
        }
        Ok(())
    }
    fn rustc_version() -> String {
        Command::new("rustc")
            .arg("--version")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|text| text.trim().into())
            .unwrap_or_else(|| "unavailable".into())
    }

    // dimensions: n, owner, logical value, retained backing, construction peak, pointer depth, seq, random.
    fn emit(
        kind: &str,
        family: &str,
        scope: &str,
        state: &str,
        dimensions: [usize; 8],
        checksum: u64,
        info: AllocationInfo,
    ) {
        let [n, owner, value, backing, peak, depth, sequential, random] = dimensions;
        println!(
            "{kind}\t{family}\t{scope}\t{n}\t{state}\t{owner}\t{value}\t{backing}\t{peak}\t{depth}\t{sequential}\t{random}\t{checksum}\t{}\t{}\t{}\t{}\t{}\t{}",
            info.count_total,
            info.count_current,
            info.count_max,
            info.bytes_total,
            info.bytes_current,
            info.bytes_max
        );
    }

    fn row_iter(n: usize) -> impl ExactSizeIterator<Item = Row> {
        (0..n).map(|i| [i as u64; 8])
    }
    fn scan(rows: &[Row]) -> u64 {
        let sequential = rows.iter().fold(0_u64, |sum, row| sum.wrapping_add(row[0]));
        let random = [0, rows.len() / 2, rows.len().saturating_sub(1)]
            .into_iter()
            .filter_map(|i| rows.get(i))
            .fold(0_u64, |sum, row| sum.wrapping_add(row[0]));
        black_box(sequential.wrapping_add(random))
    }
    fn root_entries(n: usize) -> Vec<RootEntry<ObjectDomain>> {
        (0..n)
            .map(|i| RootEntry {
                key: EntryKey::from(i as u64 + 1),
                parent: None,
                object: ObjectRef {
                    content: ContentId::from([(i % 251) as u8; 32]),
                    length: ObjectLength::from(0),
                    schema: SchemaId::Object,
                    kind: ObjectKind::from(0),
                },
            })
            .collect()
    }

    fn root_rows(n: usize) -> Result<(), RunError> {
        let (mut retained, mut peak, mut checksum, mut failure) = (0, 0, 0_u64, None);
        let info = measure(|| match GenerationRoot::new(root_entries(n)) {
            Ok(root) => {
                retained = usize::from(root.metadata_bytes());
                peak = usize::from(root.construction_peak_bytes);
                checksum = root
                    .closure()
                    .fold(0_u64, |sum, entry| sum.wrapping_add(*entry.key));
                for key in [0, n / 2, n.saturating_sub(1)] {
                    checksum = checksum.wrapping_add(u64::from(
                        root.get(EntryKey::from(key as u64 + 1)).is_some(),
                    ));
                }
                black_box(checksum);
            }
            Err(error) => failure = Some(error),
        });
        if let Some(error) = failure {
            return Err(RunError::Root(error));
        }
        emit(
            "root",
            "current_box_generation_root",
            "end_to_end_builder",
            "built",
            [
                n,
                size_of::<GenerationRoot<ObjectDomain>>(),
                retained,
                retained,
                peak,
                1,
                n,
                3,
            ],
            checksum,
            info,
        );
        boxed_rows(n);
        header_rows(n);
        thin_rows(n);
        let compact_peak = n.saturating_mul(size_of::<[u64; 9]>() + size_of::<u32>());
        emit(
            "root",
            "in_place_entry_to_row_model",
            "arithmetic_only",
            "model_only",
            [
                n,
                64,
                n * size_of::<Row>(),
                n * size_of::<[u64; 9]>(),
                compact_peak,
                1,
                n,
                3,
            ],
            0,
            AllocationInfo::default(),
        );
        Ok(())
    }
    fn boxed_rows(n: usize) {
        let mut checksum = 0;
        let info = measure(|| {
            let owner: Box<[Row]> = row_iter(n).collect();
            checksum = scan(&owner);
        });
        emit(
            "root",
            "box_slice_control",
            "retained_owner_only",
            "built",
            [
                n,
                size_of::<Box<[Row]>>(),
                n * size_of::<Row>(),
                n * size_of::<Row>(),
                n * size_of::<Row>(),
                1,
                n,
                3,
            ],
            checksum,
            info,
        );
    }
    fn header_rows(n: usize) {
        let mut checksum = 0;
        let info = measure(|| {
            let owner: HeaderRows = TriArc::from_header_and_iter([0; 6], row_iter(n));
            checksum = scan(&owner.slice);
        });
        emit(
            "root",
            "triomphe_header_slice",
            "retained_owner_only",
            "built",
            [
                n,
                size_of::<HeaderRows>(),
                n * size_of::<Row>(),
                info.bytes_max as usize,
                info.bytes_max as usize,
                1,
                n,
                3,
            ],
            checksum,
            info,
        );
    }
    fn thin_rows(n: usize) {
        let mut checksum = 0;
        let info = measure(|| {
            let owner = ThinRows::from_header_and_iter([0; 6], row_iter(n));
            checksum = owner.with_arc(|arc| scan(&arc.slice));
        });
        emit(
            "root",
            "triomphe_thin_arc",
            "retained_owner_only",
            "built",
            [
                n,
                size_of::<ThinRows>(),
                n * size_of::<Row>(),
                info.bytes_max as usize,
                info.bytes_max as usize,
                1,
                n,
                3,
            ],
            checksum,
            info,
        );
    }

    fn buckets(n: usize) -> usize {
        n.saturating_mul(2).max(1).next_power_of_two()
    }
    fn store_rows(n: usize) {
        let b = buckets(n);
        let capacity = StoreCapacity {
            bytes: 0_u64.into(),
            slots: (n as u32).into(),
        };
        let mut heap_ok = false;
        let heap = measure(|| {
            heap_ok = MemoryStore::<ObjectDomain>::new(capacity).is_ok();
        });
        let value = n * size_of::<StoreRow>() + b * size_of::<u32>();
        emit(
            "store",
            "current_heap_vec",
            "store_init",
            if heap_ok { "built" } else { "rejected" },
            [
                n,
                size_of::<MemoryStore<ObjectDomain>>(),
                value,
                heap.bytes_max as usize,
                heap.bytes_max as usize,
                1,
                n,
                3,
            ],
            0,
            heap,
        );
        let mut inline_ok = false;
        let inline = measure(|| {
            inline_ok = InlineMemoryStore::<ObjectDomain, 4, 8>::new(capacity).is_ok();
        });
        emit(
            "store",
            "current_inline_4_8",
            "store_init",
            if inline_ok { "built" } else { "rejected" },
            [
                n,
                size_of::<InlineMemoryStore<ObjectDomain, 4, 8>>(),
                value,
                0,
                0,
                0,
                n,
                3,
            ],
            0,
            inline,
        );
        borrowed_uninit_arena(n, b, value);
        bump_tables(n, b, value);
    }
    fn borrowed_uninit_arena(n: usize, b: usize, value: usize) {
        let mut entries = Vec::with_capacity(n);
        entries.resize_with(n, MaybeUninit::<StoreRow>::uninit);
        let mut index = Vec::with_capacity(b);
        index.resize_with(b, MaybeUninit::<u32>::uninit);
        let backing =
            entries.capacity() * size_of::<StoreRow>() + index.capacity() * size_of::<u32>();
        let info = measure(|| {
            black_box((&mut entries[..], &mut index[..]));
        });
        emit(
            "store",
            "borrowed_caller_uninit_arena",
            "borrowed_view_only",
            "backing_provided",
            [
                n,
                size_of::<(&mut [MaybeUninit<StoreRow>], &mut [MaybeUninit<u32>])>(),
                value,
                backing,
                backing,
                1,
                0,
                0,
            ],
            0,
            info,
        );
    }
    fn bump_tables(n: usize, b: usize, value: usize) {
        let mut checksum = 0;
        let info = measure(|| {
            let arena = Bump::new();
            let mut entries = BumpVec::with_capacity_in(n, &arena);
            entries.extend(row_iter(n));
            let mut index = BumpVec::with_capacity_in(b, &arena);
            index.resize(b, 0_u32);
            checksum =
                scan(&entries).wrapping_add(index.iter().map(|value| u64::from(*value)).sum());
            black_box(checksum);
        });
        emit(
            "store",
            "bumpalo_scoped_batch_builder",
            "scoped_batch_builder",
            "built",
            [
                n,
                size_of::<Bump>()
                    + size_of::<BumpVec<'_, StoreRow>>()
                    + size_of::<BumpVec<'_, u32>>(),
                value,
                info.bytes_max as usize,
                info.bytes_max as usize,
                1,
                n,
                3,
            ],
            checksum,
            info,
        );
    }
}

fn main() -> std::process::ExitCode {
    smoke::main()
}
