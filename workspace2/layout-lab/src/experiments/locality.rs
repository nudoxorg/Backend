//! Lab-only locality-layout candidates. They preserve a small, explicit placement vocabulary and
//! never replace the production canonical locality artifact; the packed owner is intentionally
//! unsafe research.

use core::{
    alloc::Layout,
    mem::{align_of, size_of, size_of_val},
    ptr::NonNull,
};
use std::hint::black_box;

use nudox_id::{ContentId, GenerationId, ObjectDomain};
use nudox_object::{ObjectKind, ObjectLength, ObjectRef, ProviderSet, RemoteBase};
use nudox_schema::SchemaId;

use super::{AccessFacts, AllocationFacts, CACHE_LINE_BYTES, TrackingAllocator};

const PROMISED: u8 = 1;
const OVERLAY_ABSENT: u8 = 2;
const OVERLAY_PRESENT: u8 = 3;

/// Placement output independent of any particular retained representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum LocalityTag {
    Resident,
    Promised,
    OverlayAbsent,
    OverlayPresent,
}

impl LocalityTag {
    const fn code(self) -> u64 {
        match self {
            Self::Resident => 1,
            Self::Promised => 2,
            Self::OverlayAbsent => 3,
            Self::OverlayPresent => 4,
        }
    }
}

#[derive(Clone, Copy)]
enum LocalityProfile {
    Promised,
    OverlayAbsent,
    OverlayPresent,
    Mixed,
}

impl LocalityProfile {
    const fn name(self) -> &'static str {
        match self {
            Self::Promised => "promised",
            Self::OverlayAbsent => "absent_overlay",
            Self::OverlayPresent => "present_overlay",
            Self::Mixed => "mixed",
        }
    }

    const fn tag(self, row: usize) -> LocalityTag {
        match self {
            Self::Promised => LocalityTag::Promised,
            Self::OverlayAbsent => LocalityTag::OverlayAbsent,
            Self::OverlayPresent => LocalityTag::OverlayPresent,
            Self::Mixed => match row % 4 {
                0 => LocalityTag::Resident,
                1 => LocalityTag::Promised,
                2 => LocalityTag::OverlayAbsent,
                _ => LocalityTag::OverlayPresent,
            },
        }
    }
}

#[derive(Clone, Copy)]
struct InputRow {
    tag: LocalityTag,
    provider: ProviderSet,
    object: ObjectRef<ObjectDomain>,
}

/// Logical work for rank-bearing exceptional-row membership. These counters
/// are not hardware events: they make the directory/cursor trade visible.
#[derive(Clone, Copy, Debug, Default)]
struct RankAccess {
    membership_word_reads: usize,
    rank_queries: usize,
    rank_popcount_words: usize,
    branches: usize,
    cursor_comparisons: usize,
    logical_cache_lines: usize,
}

/// Retained components of one dense row-membership index.
#[derive(Clone, Copy)]
struct RankLayoutFacts {
    membership_bytes: usize,
    directory_bytes: usize,
    omitted_zero_prefix_bytes: usize,
    tail_bits: usize,
    padding_bytes: usize,
}

/// Safe dense exception membership plus the rank needed to select a parallel
/// promise/overlay lane. Each implementation owns its bit words and directory.
trait RankIndex {
    fn member(&self, row: usize, work: &mut RankAccess) -> bool;
    fn rank(&self, row: usize, work: &mut RankAccess) -> usize;
    fn layout(&self) -> RankLayoutFacts;
}

/// The baseline requested for locality: a `u32` prefix per 256 roots, then at
/// most four 64-bit popcounts for a random rank query. The all-zero first
/// prefix is omitted rather than stored as a correlated sentinel.
struct Prefix256Rank {
    words: Box<[u64]>,
    prefixes: Box<[u32]>,
    roots: usize,
}

/// One `u32` prefix per 64 roots. It buys one-popcount random rank at a 6.25%
/// raw-bit directory cost before tail/prefix accounting.
struct WordRank {
    words: Box<[u64]>,
    prefixes: Box<[u32]>,
    roots: usize,
}

/// Rank9-style 512-bit block directory: a total rank plus packed 9-bit ranks
/// for the seven following words. Its 16-byte directory beside 64 raw bytes
/// makes the 25% directory charge explicit while retaining a one-popcount rank.
#[repr(C)]
#[derive(Clone, Copy)]
struct Rank9Directory {
    total_before_block: u64,
    word_ranks: u64,
}

const _: [(); 16] = [(); size_of::<Rank9Directory>()];

struct Rank9 {
    words: Box<[u64]>,
    directories: Box<[Rank9Directory]>,
    roots: usize,
}

/// A sparse-directory control modeled after RankSelect101111's low directory
/// overhead: one 64-bit prefix per 2,048 roots. It deliberately pays up to 32
/// word popcounts, making the storage/access crossover falsifiable.
struct Prefix2048Rank {
    words: Box<[u64]>,
    prefixes: Box<[u64]>,
    roots: usize,
}

/// Minimal borrowed view: one canonical backing slice plus validated counts.
#[repr(C)]
struct SliceAndCountsView<'bytes> {
    bytes: &'bytes [u8],
    root_count: u32,
    exception_count: u32,
    promise_count: u32,
    overlay_count: u32,
    present_count: u32,
}

/// Compact borrowed view with `u32` lane starts. It reconstructs typed lanes
/// from the one canonical byte authority instead of retaining a fat slice per
/// lane; construction validation has already checked every start/count pair.
#[repr(C)]
struct CompactLaneStartsView<'bytes> {
    bytes: &'bytes [u8],
    root_count: u32,
    exception_count: u32,
    promise_count: u32,
    overlay_count: u32,
    present_count: u32,
    exception_rows_start: u32,
    promise_bits_start: u32,
    promise_ranks_start: u32,
    providers_start: u32,
    overlay_bits_start: u32,
    overlay_ranks_start: u32,
    present_start: u32,
    basis_start_or_zero: u32,
}

/// Current-cache-shaped comparison: each borrowed lane is a fat slice and
/// layout facts are retained beside them. This measures view-header pressure,
/// not an artifact format recommendation.
#[repr(C)]
struct CachedFatSlicesView<'bytes> {
    bytes: &'bytes [u8],
    header: &'bytes [u8],
    exception_rows: &'bytes [u8],
    promise_bits: &'bytes [u8],
    promise_ranks: &'bytes [u8],
    providers: &'bytes [u8],
    overlay_bits: &'bytes [u8],
    overlay_ranks: &'bytes [u8],
    present: &'bytes [u8],
    basis: Option<&'bytes [u8; 32]>,
    root_count: u32,
    exception_count: u32,
    promise_count: u32,
    overlay_count: u32,
    present_count: u32,
    complete: usize,
}

#[derive(Clone, Copy)]
enum RankDensity {
    Empty,
    One,
    OnePercent,
    FivePercent,
    TwentyFivePercent,
    FiftyPercent,
    Full,
}

impl RankDensity {
    const fn name(self) -> &'static str {
        match self {
            Self::Empty => "0",
            Self::One => "1",
            Self::OnePercent => "1pct",
            Self::FivePercent => "5pct",
            Self::TwentyFivePercent => "25pct",
            Self::FiftyPercent => "50pct",
            Self::Full => "100pct",
        }
    }

    const fn exceptions(self, roots: usize) -> usize {
        match self {
            Self::Empty => 0,
            Self::One => {
                if roots == 0 {
                    0
                } else {
                    1
                }
            }
            Self::OnePercent => roots / 100,
            Self::FivePercent => roots / 20,
            Self::TwentyFivePercent => roots / 4,
            Self::FiftyPercent => roots / 2,
            Self::Full => roots,
        }
    }
}

/// Current three-sidecar model: compact route, provider payloads, and full remote-base payloads.
#[repr(C)]
#[derive(Clone, Copy)]
struct CurrentRoute {
    row: u32,
    payload: u32,
    tag: u8,
    padding: [u8; 3],
}

struct CurrentLocality {
    generation: GenerationId,
    routes: Box<[CurrentRoute]>,
    promises: Box<[ProviderSet]>,
    overlays: Box<[RemoteBase<ObjectDomain>]>,
}

/// Dense candidate: one fused enum row per root coordinate.
#[derive(Clone, Copy)]
enum FusedRow {
    Resident,
    Promised(ProviderSet),
    Overlay(RemoteBase<ObjectDomain>),
}

struct FusedLocality {
    generation: GenerationId,
    rows: Box<[FusedRow]>,
}

/// Sparse candidate that stores providers inline and puts only present-overlay descriptors in a
/// sidecar. Every overlay shares the enclosing generation rather than repeating it in `RemoteBase`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SplitRoute {
    row: u32,
    tag: u8,
    padding: [u8; 3],
    payload: u64,
}

/// The 13-byte grammar candidate from `PACKED_COLLECTIONS.md`: a packed row coordinate, closed
/// tag, and inline provider-or-present-object payload. Its unaligned loads are deliberately
/// measured as a separate unsafe lab candidate rather than assumed to beat the aligned 16-byte row.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct PackedRoute13 {
    row: u32,
    tag: u8,
    payload: u64,
}

impl PackedRoute13 {
    fn row(self) -> u32 {
        // SAFETY: `repr(packed)` permits byte alignment only; reading through `addr_of!` avoids
        // creating an unaligned reference and returns the initialized copied field by value.
        unsafe { core::ptr::addr_of!(self.row).read_unaligned() }
    }

    fn tag(self) -> u8 {
        self.tag
    }

    fn payload(self) -> u64 {
        // SAFETY: same packed-field proof as `row` for the initialized payload field.
        unsafe { core::ptr::addr_of!(self.payload).read_unaligned() }
    }
}

struct SplitLocality {
    generation: GenerationId,
    routes: Box<[SplitRoute]>,
    present_objects: Box<[ObjectRef<ObjectDomain>]>,
}

struct Packed13Locality {
    generation: GenerationId,
    routes: Box<[PackedRoute13]>,
    present_objects: Box<[ObjectRef<ObjectDomain>]>,
}

/// One packed byte allocation containing an exact generation, compact routes, provider sidecars,
/// and remote bases. This is a lab-only unsafe owner; its proof/tooling requirements are recorded
/// in the audit and it is deliberately absent from production crates.
struct PackedLocality {
    pointer: NonNull<u8>,
    allocation: Layout,
    generation_offset: usize,
    routes_offset: usize,
    routes_len: usize,
    promises_offset: usize,
    promises_len: usize,
    overlays_offset: usize,
    overlays_len: usize,
    padding_bytes: usize,
}

struct CallerView<'input> {
    tags: &'input [LocalityTag],
}

/// Borrow-only adapter over separately leased normalized regions. It neither assumes a common
/// allocation nor owns the borrowed route/object buffers.
struct LeasedRegionsView<'header, 'routes, 'objects> {
    generation: &'header GenerationId,
    routes: &'routes [SplitRoute],
    present_objects: &'objects [ObjectRef<ObjectDomain>],
}

#[derive(Clone, Copy)]
struct LocalityFacts {
    representation_bytes: usize,
    caller_bytes: usize,
    padding_bytes: usize,
    remote_base_bytes: usize,
    repeated_generation_bytes: usize,
    scan: AccessFacts,
    random: AccessFacts,
    exact: bool,
}

fn provider() -> Result<ProviderSet, nudox_object::ProviderIdError> {
    nudox_object::ProviderId::try_from(7_u8).map(ProviderSet::only)
}

fn generation() -> GenerationId {
    GenerationId::from_digest([0x6a; 32])
}

fn object(row: usize) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::from_digest([row as u8; 32]),
        length: ObjectLength::from(1),
        schema: SchemaId::Object,
        kind: ObjectKind::from(7),
    }
}

fn input(
    rows: usize,
    profile: LocalityProfile,
) -> Result<Vec<InputRow>, nudox_object::ProviderIdError> {
    let provider = provider()?;
    Ok((0..rows)
        .map(|row| InputRow {
            tag: profile.tag(row),
            provider,
            object: object(row),
        })
        .collect())
}

fn checksum(tags: impl Iterator<Item = LocalityTag>) -> u64 {
    tags.enumerate().fold(0_u64, |state, (row, tag)| {
        state
            .wrapping_mul(131)
            .wrapping_add((row as u64).wrapping_mul(17).wrapping_add(tag.code()))
    })
}

fn expected_checksum(input: &[InputRow]) -> u64 {
    checksum(input.iter().map(|row| row.tag))
}

fn line_count(bytes: usize) -> usize {
    bytes.div_ceil(CACHE_LINE_BYTES)
}

const fn bool_count(value: bool) -> usize {
    if value { 1 } else { 0 }
}

fn random_positions(rows: usize) -> ([usize; 3], usize) {
    match rows {
        0 => ([0; 3], 0),
        1 => ([0; 3], 1),
        _ => ([0, rows / 2, rows - 1], 3),
    }
}

fn exception_rows(roots: usize, exceptions: usize) -> Vec<u32> {
    let mut rows = Vec::with_capacity(exceptions);
    for ordinal in 0..exceptions {
        rows.push((ordinal * roots / exceptions) as u32);
    }
    rows
}

fn membership_words(roots: usize, exceptions: &[u32]) -> Box<[u64]> {
    let mut words = vec![0_u64; roots.div_ceil(64)];
    for row in exceptions {
        let row = *row as usize;
        words[row / 64] |= 1_u64 << (row % 64);
    }
    words.into_boxed_slice()
}

const fn lower_bits(bits: usize) -> u64 {
    if bits == 0 { 0 } else { (1_u64 << bits) - 1 }
}

fn member_word(words: &[u64], row: usize, work: &mut RankAccess) -> bool {
    work.membership_word_reads += 1;
    work.branches += 1;
    words
        .get(row / 64)
        .is_some_and(|word| (word >> (row % 64)) & 1 == 1)
}

fn count_words_before(
    words: &[u64],
    first_word: usize,
    current_word: usize,
    current_bit: usize,
    work: &mut RankAccess,
) -> usize {
    let mut rank = 0;
    for word in &words[first_word..current_word] {
        work.rank_popcount_words += 1;
        rank += word.count_ones() as usize;
    }
    if current_bit != 0 {
        work.rank_popcount_words += 1;
        rank += (words[current_word] & lower_bits(current_bit)).count_ones() as usize;
    }
    rank
}

impl Prefix256Rank {
    fn build(roots: usize, exceptions: &[u32]) -> Self {
        let words = membership_words(roots, exceptions);
        let blocks = roots.div_ceil(256);
        let mut prefixes = Vec::with_capacity(blocks.saturating_sub(1));
        let mut total = 0_u32;
        for block in 0..blocks {
            if block != 0 {
                prefixes.push(total);
            }
            let first_word = block * 4;
            let last_word = (first_word + 4).min(words.len());
            for word in &words[first_word..last_word] {
                total += word.count_ones();
            }
        }
        Self {
            words,
            prefixes: prefixes.into_boxed_slice(),
            roots,
        }
    }
}

impl RankIndex for Prefix256Rank {
    fn member(&self, row: usize, work: &mut RankAccess) -> bool {
        member_word(&self.words, row, work)
    }

    fn rank(&self, row: usize, work: &mut RankAccess) -> usize {
        work.rank_queries += 1;
        let block = row / 256;
        let before_block = if block == 0 {
            0
        } else {
            self.prefixes[block - 1] as usize
        };
        before_block + count_words_before(&self.words, block * 4, row / 64, row % 64, work)
    }

    fn layout(&self) -> RankLayoutFacts {
        RankLayoutFacts {
            membership_bytes: self.words.len() * size_of::<u64>(),
            directory_bytes: self.prefixes.len() * size_of::<u32>(),
            omitted_zero_prefix_bytes: bool_count(!self.words.is_empty()) * size_of::<u32>(),
            tail_bits: self.words.len() * 64 - self.roots,
            padding_bytes: 0,
        }
    }
}

impl WordRank {
    fn build(roots: usize, exceptions: &[u32]) -> Self {
        let words = membership_words(roots, exceptions);
        let mut prefixes = Vec::with_capacity(words.len().saturating_sub(1));
        let mut total = 0_u32;
        for (index, word) in words.iter().enumerate() {
            if index != 0 {
                prefixes.push(total);
            }
            total += word.count_ones();
        }
        Self {
            words,
            prefixes: prefixes.into_boxed_slice(),
            roots,
        }
    }
}

impl RankIndex for WordRank {
    fn member(&self, row: usize, work: &mut RankAccess) -> bool {
        member_word(&self.words, row, work)
    }

    fn rank(&self, row: usize, work: &mut RankAccess) -> usize {
        work.rank_queries += 1;
        let word = row / 64;
        let before_word = if word == 0 {
            0
        } else {
            self.prefixes[word - 1] as usize
        };
        before_word + count_words_before(&self.words, word, word, row % 64, work)
    }

    fn layout(&self) -> RankLayoutFacts {
        RankLayoutFacts {
            membership_bytes: self.words.len() * size_of::<u64>(),
            directory_bytes: self.prefixes.len() * size_of::<u32>(),
            omitted_zero_prefix_bytes: bool_count(!self.words.is_empty()) * size_of::<u32>(),
            tail_bits: self.words.len() * 64 - self.roots,
            padding_bytes: 0,
        }
    }
}

impl Rank9 {
    fn build(roots: usize, exceptions: &[u32]) -> Self {
        let words = membership_words(roots, exceptions);
        let blocks = roots.div_ceil(512);
        let mut directories = Vec::with_capacity(blocks);
        let mut total = 0_u64;
        for block in 0..blocks {
            let first_word = block * 8;
            let last_word = (first_word + 8).min(words.len());
            let mut word_ranks = 0_u64;
            let mut within_block = 0_u64;
            for (ordinal, word) in words[first_word..last_word].iter().enumerate() {
                if ordinal != 0 {
                    word_ranks |= within_block << ((ordinal - 1) * 9);
                }
                within_block += u64::from(word.count_ones());
            }
            directories.push(Rank9Directory {
                total_before_block: total,
                word_ranks,
            });
            total += within_block;
        }
        Self {
            words,
            directories: directories.into_boxed_slice(),
            roots,
        }
    }
}

impl RankIndex for Rank9 {
    fn member(&self, row: usize, work: &mut RankAccess) -> bool {
        member_word(&self.words, row, work)
    }

    fn rank(&self, row: usize, work: &mut RankAccess) -> usize {
        work.rank_queries += 1;
        let block = row / 512;
        let word_in_block = (row % 512) / 64;
        let directory = self.directories[block];
        let before_word = if word_in_block == 0 {
            0
        } else {
            ((directory.word_ranks >> ((word_in_block - 1) * 9)) & 0x1ff) as usize
        };
        directory.total_before_block as usize
            + before_word
            + count_words_before(&self.words, row / 64, row / 64, row % 64, work)
    }

    fn layout(&self) -> RankLayoutFacts {
        RankLayoutFacts {
            membership_bytes: self.words.len() * size_of::<u64>(),
            directory_bytes: self.directories.len() * size_of::<Rank9Directory>(),
            omitted_zero_prefix_bytes: 0,
            tail_bits: self.words.len() * 64 - self.roots,
            padding_bytes: 0,
        }
    }
}

impl Prefix2048Rank {
    fn build(roots: usize, exceptions: &[u32]) -> Self {
        let words = membership_words(roots, exceptions);
        let blocks = roots.div_ceil(2_048);
        let mut prefixes = Vec::with_capacity(blocks.saturating_sub(1));
        let mut total = 0_u64;
        for block in 0..blocks {
            if block != 0 {
                prefixes.push(total);
            }
            let first_word = block * 32;
            let last_word = (first_word + 32).min(words.len());
            for word in &words[first_word..last_word] {
                total += u64::from(word.count_ones());
            }
        }
        Self {
            words,
            prefixes: prefixes.into_boxed_slice(),
            roots,
        }
    }
}

impl RankIndex for Prefix2048Rank {
    fn member(&self, row: usize, work: &mut RankAccess) -> bool {
        member_word(&self.words, row, work)
    }

    fn rank(&self, row: usize, work: &mut RankAccess) -> usize {
        work.rank_queries += 1;
        let block = row / 2_048;
        let before_block = if block == 0 {
            0
        } else {
            self.prefixes[block - 1] as usize
        };
        before_block + count_words_before(&self.words, block * 32, row / 64, row % 64, work)
    }

    fn layout(&self) -> RankLayoutFacts {
        RankLayoutFacts {
            membership_bytes: self.words.len() * size_of::<u64>(),
            directory_bytes: self.prefixes.len() * size_of::<u64>(),
            omitted_zero_prefix_bytes: bool_count(!self.words.is_empty()) * size_of::<u64>(),
            tail_bits: self.words.len() * 64 - self.roots,
            padding_bytes: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct RankEvidence {
    layout: RankLayoutFacts,
    exact: bool,
    sequential: RankAccess,
    random: RankAccess,
}

fn sequential_rank_cursor<IndexRepresentation: RankIndex>(
    index: &IndexRepresentation,
    roots: usize,
    exceptions: &[u32],
) -> (bool, RankAccess) {
    let mut work = RankAccess::default();
    let mut cursor = 0;
    let mut exact = true;
    for row in 0..roots {
        let member = index.member(row, &mut work);
        work.cursor_comparisons += 1;
        let expected = exceptions
            .get(cursor)
            .is_some_and(|exception| *exception as usize == row);
        exact &= member == expected;
        if member {
            cursor += 1;
        }
    }
    exact &= cursor == exceptions.len();
    (exact, work)
}

fn random_rank_queries<IndexRepresentation: RankIndex>(
    index: &IndexRepresentation,
    roots: usize,
    exceptions: &[u32],
) -> (bool, RankAccess) {
    let (positions, count) = random_positions(roots);
    let mut work = RankAccess::default();
    let mut exact = true;
    for row in positions.into_iter().take(count) {
        let member = index.member(row, &mut work);
        let rank = index.rank(row, &mut work);
        let expected_rank = exceptions.partition_point(|exception| (*exception as usize) < row);
        let expected_member = exceptions
            .get(expected_rank)
            .is_some_and(|exception| *exception as usize == row);
        exact &= member == expected_member && rank == expected_rank;
    }
    (exact, work)
}

fn rank_evidence<IndexRepresentation: RankIndex>(
    index: &IndexRepresentation,
    roots: usize,
    exceptions: &[u32],
) -> RankEvidence {
    let layout = index.layout();
    let (sequential_exact, mut sequential) = sequential_rank_cursor(index, roots, exceptions);
    let (random_exact, mut random) = random_rank_queries(index, roots, exceptions);
    let footprint = layout.membership_bytes + layout.directory_bytes;
    sequential.logical_cache_lines = line_count(footprint);
    random.logical_cache_lines = random.membership_word_reads + random.rank_popcount_words;
    RankEvidence {
        layout,
        exact: sequential_exact && random_exact && sequential.rank_queries == 0,
        sequential,
        random,
    }
}

fn print_rank_row(
    representation: &str,
    roots: usize,
    density: RankDensity,
    exceptions: usize,
    evidence: RankEvidence,
    allocation: AllocationFacts,
) {
    println!(
        "locality_dense_exception_rank\t{representation}\t{roots}\t{}\t{exceptions}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        density.name(),
        bool_count(evidence.exact),
        evidence.layout.membership_bytes,
        evidence.layout.directory_bytes,
        evidence.layout.omitted_zero_prefix_bytes,
        evidence.layout.tail_bits,
        evidence.layout.padding_bytes,
        evidence.sequential.membership_word_reads,
        evidence.sequential.cursor_comparisons,
        evidence.sequential.rank_queries,
        evidence.sequential.rank_popcount_words,
        evidence.sequential.branches,
        evidence.sequential.logical_cache_lines,
        evidence.random.membership_word_reads,
        evidence.random.rank_queries,
        evidence.random.rank_popcount_words,
        evidence.random.branches,
        evidence.random.logical_cache_lines,
        allocation.allocations,
        allocation.allocated_bytes,
        allocation.peak_live_bytes,
        allocation.deallocations,
    );
}

fn dense_exception_rank_rows(allocator: &TrackingAllocator) {
    println!(
        "columns\tlocality_dense_exception_rank\trepresentation\troots\tdensity\texceptions\texact\tmembership_bytes\tdirectory_bytes\tomitted_zero_prefix_bytes\ttail_bits\tpadding_bytes\tsequential_membership_word_reads\tsequential_cursor_comparisons\tsequential_rank_queries\tsequential_rank_popcount_words\tsequential_branches\tsequential_logical_cache_lines\trandom_membership_word_reads\trandom_rank_queries\trandom_rank_popcount_words\trandom_branches\trandom_logical_cache_lines\tallocations\tallocated_bytes\tpeak_live_bytes\tdrop_deallocations"
    );
    println!(
        "locality_dense_exception_rank_type\tRank9Directory\tbytes\t{}\tPrefix256Stride\troots\t256\tPrefix2048Stride\troots\t2048",
        size_of::<Rank9Directory>(),
    );
    for roots in [0_usize, 1, 100_000] {
        let densities: &[RankDensity] = match roots {
            0 => &[RankDensity::Empty],
            1 => &[RankDensity::Empty, RankDensity::Full],
            _ => &[
                RankDensity::Empty,
                RankDensity::One,
                RankDensity::OnePercent,
                RankDensity::FivePercent,
                RankDensity::TwentyFivePercent,
                RankDensity::FiftyPercent,
                RankDensity::Full,
            ],
        };
        for density in densities {
            let exceptions = density.exceptions(roots);
            let rows = exception_rows(roots, exceptions);
            let (evidence, allocation) = allocator.measure(|| {
                let index = Prefix256Rank::build(roots, &rows);
                rank_evidence(&index, roots, &rows)
            });
            print_rank_row(
                "u32_prefix_per_256_bounded_four_popcounts",
                roots,
                *density,
                exceptions,
                evidence,
                allocation,
            );
            let (evidence, allocation) = allocator.measure(|| {
                let index = WordRank::build(roots, &rows);
                rank_evidence(&index, roots, &rows)
            });
            print_rank_row(
                "u32_prefix_per_64_one_popcount",
                roots,
                *density,
                exceptions,
                evidence,
                allocation,
            );
            let (evidence, allocation) = allocator.measure(|| {
                let index = Rank9::build(roots, &rows);
                rank_evidence(&index, roots, &rows)
            });
            print_rank_row(
                "rank9_512bit_interleaved_directory",
                roots,
                *density,
                exceptions,
                evidence,
                allocation,
            );
            let (evidence, allocation) = allocator.measure(|| {
                let index = Prefix2048Rank::build(roots, &rows);
                rank_evidence(&index, roots, &rows)
            });
            print_rank_row(
                "rankselect101111_sparse_directory_model",
                roots,
                *density,
                exceptions,
                evidence,
                allocation,
            );
        }
    }
}

/// The production rank domains begin after root membership: one bit per
/// exception selects promise versus overlay, and one bit per overlay selects
/// whether its descriptor lane is present. Root membership remains sorted
/// `u32` exception rows, not a dense root-coordinate rank index.
struct ProductionRankWorkload {
    exception_rows: Box<[u32]>,
    promise_ordinals: Box<[u32]>,
    present_overlay_ordinals: Box<[u32]>,
}

impl ProductionRankWorkload {
    fn build(roots: usize, density: RankDensity) -> Self {
        let exception_rows = exception_rows(roots, density.exceptions(roots));
        let mut promise_ordinals = Vec::with_capacity(exception_rows.len().div_ceil(2));
        let mut present_overlay_ordinals = Vec::with_capacity(exception_rows.len() / 4);
        let mut overlays = 0_usize;
        for exception_ordinal in 0..exception_rows.len() {
            if exception_ordinal.is_multiple_of(2) {
                promise_ordinals.push(exception_ordinal as u32);
            } else {
                if overlays.is_multiple_of(2) {
                    present_overlay_ordinals.push(overlays as u32);
                }
                overlays += 1;
            }
        }
        Self {
            exception_rows: exception_rows.into_boxed_slice(),
            promise_ordinals: promise_ordinals.into_boxed_slice(),
            present_overlay_ordinals: present_overlay_ordinals.into_boxed_slice(),
        }
    }

    fn exception_count(&self) -> usize {
        self.exception_rows.len()
    }

    fn overlay_count(&self) -> usize {
        self.exception_count() - self.promise_ordinals.len()
    }
}

#[derive(Clone, Copy)]
struct ProductionRankLayout {
    exception_row_bytes: usize,
    class: RankLayoutFacts,
    overlay_presence: RankLayoutFacts,
}

impl ProductionRankLayout {
    fn total_artifact_bytes(self) -> usize {
        self.exception_row_bytes
            + self.class.membership_bytes
            + self.class.directory_bytes
            + self.overlay_presence.membership_bytes
            + self.overlay_presence.directory_bytes
    }

    fn omitted_zero_prefix_bytes(self) -> usize {
        self.class.omitted_zero_prefix_bytes + self.overlay_presence.omitted_zero_prefix_bytes
    }

    fn tail_bits(self) -> usize {
        self.class.tail_bits + self.overlay_presence.tail_bits
    }

    fn padding_bytes(self) -> usize {
        self.class.padding_bytes + self.overlay_presence.padding_bytes
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ProductionRankAccess {
    exception_binary_comparisons: usize,
    exception_cursor_comparisons: usize,
    class: RankAccess,
    overlay_presence: RankAccess,
}

#[derive(Clone, Copy)]
struct ProductionRankEvidence {
    layout: ProductionRankLayout,
    counts: (usize, usize, usize, usize),
    exact: bool,
    sequential: ProductionRankAccess,
    random: ProductionRankAccess,
}

fn production_rank_evidence<IndexRepresentation: RankIndex>(
    workload: &ProductionRankWorkload,
    roots: usize,
    class: &IndexRepresentation,
    overlay_presence: &IndexRepresentation,
) -> ProductionRankEvidence {
    let layout = ProductionRankLayout {
        exception_row_bytes: workload.exception_rows.len() * size_of::<u32>(),
        class: class.layout(),
        overlay_presence: overlay_presence.layout(),
    };
    let (sequential_exact, mut sequential) =
        sequential_production_cursor(workload, roots, class, overlay_presence);
    let (random_exact, mut random) =
        random_production_queries(workload, roots, class, overlay_presence);
    sequential.class.logical_cache_lines =
        line_count(layout.class.membership_bytes + layout.class.directory_bytes);
    sequential.overlay_presence.logical_cache_lines = line_count(
        layout.overlay_presence.membership_bytes + layout.overlay_presence.directory_bytes,
    );
    random.class.logical_cache_lines =
        random.class.membership_word_reads + random.class.rank_popcount_words;
    random.overlay_presence.logical_cache_lines =
        random.overlay_presence.membership_word_reads + random.overlay_presence.rank_popcount_words;
    ProductionRankEvidence {
        layout,
        counts: (
            workload.exception_count(),
            workload.promise_ordinals.len(),
            workload.overlay_count(),
            workload.present_overlay_ordinals.len(),
        ),
        exact: sequential_exact
            && random_exact
            && sequential.class.rank_queries == 0
            && sequential.overlay_presence.rank_queries == 0,
        sequential,
        random,
    }
}

fn sequential_production_cursor<IndexRepresentation: RankIndex>(
    workload: &ProductionRankWorkload,
    roots: usize,
    class: &IndexRepresentation,
    overlay_presence: &IndexRepresentation,
) -> (bool, ProductionRankAccess) {
    let mut access = ProductionRankAccess::default();
    let mut exception_cursor = 0;
    let mut promise_cursor = 0;
    let mut overlay_cursor = 0;
    let mut present_cursor = 0;
    let mut exact = true;
    for row in 0..roots {
        access.exception_cursor_comparisons += 1;
        let is_exception = workload
            .exception_rows
            .get(exception_cursor)
            .is_some_and(|exception| *exception as usize == row);
        if is_exception {
            let is_promise = class.member(exception_cursor, &mut access.class);
            let expected_promise = workload
                .promise_ordinals
                .get(promise_cursor)
                .is_some_and(|ordinal| *ordinal as usize == exception_cursor);
            exact &= is_promise == expected_promise;
            if is_promise {
                promise_cursor += 1;
            } else {
                let (overlay_exact, is_present) = sequential_overlay_member(
                    workload,
                    overlay_presence,
                    overlay_cursor,
                    present_cursor,
                    &mut access,
                );
                exact &= overlay_exact;
                if is_present {
                    present_cursor += 1;
                }
                overlay_cursor += 1;
            }
            exception_cursor += 1;
        }
    }
    exact &= exception_cursor == workload.exception_count();
    exact &= promise_cursor == workload.promise_ordinals.len();
    exact &= overlay_cursor == workload.overlay_count();
    exact &= present_cursor == workload.present_overlay_ordinals.len();
    (exact, access)
}

fn sequential_overlay_member<IndexRepresentation: RankIndex>(
    workload: &ProductionRankWorkload,
    overlay_presence: &IndexRepresentation,
    overlay_cursor: usize,
    present_cursor: usize,
    access: &mut ProductionRankAccess,
) -> (bool, bool) {
    let actual = overlay_presence.member(overlay_cursor, &mut access.overlay_presence);
    let expected = workload
        .present_overlay_ordinals
        .get(present_cursor)
        .is_some_and(|ordinal| *ordinal as usize == overlay_cursor);
    (actual == expected, actual)
}

fn exception_ordinal_at_row(
    exception_rows: &[u32],
    row: usize,
    access: &mut ProductionRankAccess,
) -> Option<usize> {
    let mut lower = 0;
    let mut upper = exception_rows.len();
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        let candidate = *exception_rows.get(middle)?;
        access.exception_binary_comparisons += 1;
        if candidate < row as u32 {
            lower = middle + 1;
        } else {
            upper = middle;
        }
    }
    exception_rows
        .get(lower)
        .is_some_and(|candidate| *candidate as usize == row)
        .then_some(lower)
}

fn selected_before(selected: &[u32], ordinal: usize) -> usize {
    selected.partition_point(|selected_ordinal| (*selected_ordinal as usize) < ordinal)
}

fn selected_ordinal(selected: &[u32], ordinal: usize) -> bool {
    selected
        .get(selected_before(selected, ordinal))
        .is_some_and(|selected_ordinal| *selected_ordinal as usize == ordinal)
}

fn random_production_queries<IndexRepresentation: RankIndex>(
    workload: &ProductionRankWorkload,
    roots: usize,
    class: &IndexRepresentation,
    overlay_presence: &IndexRepresentation,
) -> (bool, ProductionRankAccess) {
    let (positions, count) = random_positions(roots);
    let mut access = ProductionRankAccess::default();
    let mut exact = true;
    for row in positions.into_iter().take(count) {
        let expected = selected_before(&workload.exception_rows, row);
        let actual = exception_ordinal_at_row(&workload.exception_rows, row, &mut access);
        let expected_member = workload
            .exception_rows
            .get(expected)
            .is_some_and(|exception| *exception as usize == row);
        exact &= actual.is_some() == expected_member;
        if let Some(exception_ordinal) = actual {
            exact &= random_exception_lanes(
                workload,
                class,
                overlay_presence,
                exception_ordinal,
                &mut access,
            );
        }
    }
    (exact, access)
}

fn random_exception_lanes<IndexRepresentation: RankIndex>(
    workload: &ProductionRankWorkload,
    class: &IndexRepresentation,
    overlay_presence: &IndexRepresentation,
    exception_ordinal: usize,
    access: &mut ProductionRankAccess,
) -> bool {
    let is_promise = class.member(exception_ordinal, &mut access.class);
    let promise_ordinal = class.rank(exception_ordinal, &mut access.class);
    let expected_promise = selected_ordinal(&workload.promise_ordinals, exception_ordinal);
    let expected_promise_ordinal = selected_before(&workload.promise_ordinals, exception_ordinal);
    if is_promise {
        is_promise == expected_promise && promise_ordinal == expected_promise_ordinal
    } else {
        let overlay_ordinal = exception_ordinal - promise_ordinal;
        let is_present = overlay_presence.member(overlay_ordinal, &mut access.overlay_presence);
        let present_ordinal = overlay_presence.rank(overlay_ordinal, &mut access.overlay_presence);
        let expected_present =
            selected_ordinal(&workload.present_overlay_ordinals, overlay_ordinal);
        let expected_present_ordinal =
            selected_before(&workload.present_overlay_ordinals, overlay_ordinal);
        !expected_promise
            && promise_ordinal == expected_promise_ordinal
            && is_present == expected_present
            && present_ordinal == expected_present_ordinal
    }
}

fn print_production_rank_row(
    representation: &str,
    roots: usize,
    density: RankDensity,
    evidence: ProductionRankEvidence,
    allocation: AllocationFacts,
) {
    let (exceptions, promises, overlays, present) = evidence.counts;
    println!(
        "locality_production_rank\t{representation}\t{roots}\t{}\t{exceptions}\t{promises}\t{overlays}\t{present}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        density.name(),
        bool_count(evidence.exact),
        evidence.layout.exception_row_bytes,
        evidence.layout.class.membership_bytes,
        evidence.layout.class.directory_bytes,
        evidence.layout.overlay_presence.membership_bytes,
        evidence.layout.overlay_presence.directory_bytes,
        evidence.layout.total_artifact_bytes(),
        evidence.layout.omitted_zero_prefix_bytes(),
        evidence.layout.tail_bits(),
        evidence.layout.padding_bytes(),
        evidence.sequential.exception_cursor_comparisons,
        evidence.sequential.class.membership_word_reads,
        evidence.sequential.class.rank_queries,
        evidence.sequential.overlay_presence.membership_word_reads,
        evidence.sequential.overlay_presence.rank_queries,
        evidence.random.exception_binary_comparisons,
        evidence.random.class.membership_word_reads,
        evidence.random.class.rank_queries,
        evidence.random.class.rank_popcount_words,
        evidence.random.overlay_presence.membership_word_reads,
        evidence.random.overlay_presence.rank_queries,
        evidence.random.overlay_presence.rank_popcount_words,
        allocation.allocations,
        allocation.allocated_bytes,
        format_args!(
            "{}\t{}",
            allocation.peak_live_bytes, allocation.deallocations
        ),
    );
}

fn print_production_candidate<IndexRepresentation: RankIndex>(
    allocator: &TrackingAllocator,
    representation: &str,
    roots: usize,
    density: RankDensity,
    build: fn(usize, &[u32]) -> IndexRepresentation,
) {
    let (evidence, allocation) = allocator.measure(|| {
        let workload = ProductionRankWorkload::build(roots, density);
        let class = build(workload.exception_count(), &workload.promise_ordinals);
        let overlay_presence = build(workload.overlay_count(), &workload.present_overlay_ordinals);
        production_rank_evidence(&workload, roots, &class, &overlay_presence)
    });
    print_production_rank_row(representation, roots, density, evidence, allocation);
}

#[cfg(test)]
fn production_rank_is_exact<IndexRepresentation: RankIndex>(
    roots: usize,
    density: RankDensity,
    build: fn(usize, &[u32]) -> IndexRepresentation,
) -> bool {
    let workload = ProductionRankWorkload::build(roots, density);
    let class = build(workload.exception_count(), &workload.promise_ordinals);
    let overlay_presence = build(workload.overlay_count(), &workload.present_overlay_ordinals);
    production_rank_evidence(&workload, roots, &class, &overlay_presence).exact
}

fn production_rank_rows(allocator: &TrackingAllocator) {
    println!(
        "columns\tlocality_production_rank\trepresentation\troots\texception_density\texceptions\tpromises\toverlays\tpresent_overlays\texact\texception_row_bytes\tclass_membership_bytes\tclass_directory_bytes\toverlay_membership_bytes\toverlay_directory_bytes\tfull_artifact_bytes\tomitted_zero_prefix_bytes\ttail_bits\tpadding_bytes\tsequential_exception_cursor_comparisons\tsequential_class_membership_word_reads\tsequential_class_rank_queries\tsequential_overlay_membership_word_reads\tsequential_overlay_rank_queries\trandom_exception_binary_comparisons\trandom_class_membership_word_reads\trandom_class_rank_queries\trandom_class_rank_popcount_words\trandom_overlay_membership_word_reads\trandom_overlay_rank_queries\trandom_overlay_rank_popcount_words\tallocations\tallocated_bytes\tpeak_live_bytes\tdrop_deallocations"
    );
    println!(
        "locality_production_rank_type\texception_rows\tsorted_u32\trandom_membership\tlower_bound\tclass_pattern\talternating_promise_overlay\toverlay_presence_pattern\talternating"
    );
    for roots in [0_usize, 1, 100_000] {
        let densities: &[RankDensity] = match roots {
            0 => &[RankDensity::Empty],
            1 => &[RankDensity::Empty, RankDensity::Full],
            _ => &[
                RankDensity::Empty,
                RankDensity::One,
                RankDensity::OnePercent,
                RankDensity::FivePercent,
                RankDensity::TwentyFivePercent,
                RankDensity::FiftyPercent,
                RankDensity::Full,
            ],
        };
        for density in densities {
            print_production_candidate(
                allocator,
                "u32_prefix_per_256_bounded_four_popcounts",
                roots,
                *density,
                Prefix256Rank::build,
            );
            print_production_candidate(
                allocator,
                "u32_prefix_per_64_one_popcount",
                roots,
                *density,
                WordRank::build,
            );
            print_production_candidate(
                allocator,
                "rank9_512bit_interleaved_directory",
                roots,
                *density,
                Rank9::build,
            );
            print_production_candidate(
                allocator,
                "rankselect101111_sparse_directory_model",
                roots,
                *density,
                Prefix2048Rank::build,
            );
        }
    }
}

fn view_header_rows() {
    println!(
        "columns\tlocality_view_header\trepresentation\theader_bytes\tpointer_depth\treconstruction_work"
    );
    println!(
        "locality_view_header\tslice_plus_counts\t{}\t1\treborrow_lanes_from_validated_counts",
        size_of::<SliceAndCountsView<'static>>(),
    );
    println!(
        "locality_view_header\tslice_plus_compact_u32_lane_starts\t{}\t1\treborrow_one_lane_from_checked_start_count",
        size_of::<CompactLaneStartsView<'static>>(),
    );
    println!(
        "locality_view_header\tcached_fat_slices\t{}\t1\tno_lane_reborrow_but_retains_nine_fat_slices",
        size_of::<CachedFatSlicesView<'static>>(),
    );
}

type SparseParts = (
    Vec<CurrentRoute>,
    Vec<ProviderSet>,
    Vec<RemoteBase<ObjectDomain>>,
);

fn sparse_parts(input: &[InputRow]) -> SparseParts {
    let mut routes = Vec::with_capacity(input.len());
    let mut promises = Vec::with_capacity(input.len());
    let mut overlays = Vec::with_capacity(input.len());
    let shared_generation = generation();
    for (row, input) in input.iter().enumerate() {
        let row = row as u32;
        let remote = match input.tag {
            LocalityTag::Resident => continue,
            LocalityTag::Promised => {
                let payload = promises.len() as u32;
                promises.push(input.provider);
                routes.push(CurrentRoute {
                    row,
                    payload,
                    tag: PROMISED,
                    padding: [0; 3],
                });
                continue;
            }
            LocalityTag::OverlayAbsent => RemoteBase::Absent {
                generation: shared_generation,
            },
            LocalityTag::OverlayPresent => RemoteBase::Present {
                generation: shared_generation,
                object: input.object,
            },
        };
        let payload = overlays.len() as u32;
        overlays.push(remote);
        routes.push(CurrentRoute {
            row,
            payload,
            tag: OVERLAY_ABSENT,
            padding: [0; 3],
        });
    }
    (routes, promises, overlays)
}

fn tag_from_remote(remote: RemoteBase<ObjectDomain>) -> LocalityTag {
    match remote {
        RemoteBase::Absent { .. } => LocalityTag::OverlayAbsent,
        RemoteBase::Present { .. } => LocalityTag::OverlayPresent,
    }
}

fn split_parts(input: &[InputRow]) -> (Vec<SplitRoute>, Vec<ObjectRef<ObjectDomain>>) {
    let mut routes = Vec::with_capacity(input.len());
    let mut present_objects = Vec::with_capacity(input.len());
    for (row, input) in input.iter().enumerate() {
        let (tag, payload) = match input.tag {
            LocalityTag::Resident => continue,
            LocalityTag::Promised => (PROMISED, *input.provider),
            LocalityTag::OverlayAbsent => (OVERLAY_ABSENT, 0),
            LocalityTag::OverlayPresent => {
                let index = present_objects.len() as u64;
                present_objects.push(input.object);
                (OVERLAY_PRESENT, index)
            }
        };
        routes.push(SplitRoute {
            row: row as u32,
            tag,
            padding: [0; 3],
            payload,
        });
    }
    (routes, present_objects)
}

fn tag_from_split(route: SplitRoute, present_objects: &[ObjectRef<ObjectDomain>]) -> LocalityTag {
    match route.tag {
        PROMISED => LocalityTag::Promised,
        OVERLAY_ABSENT => LocalityTag::OverlayAbsent,
        OVERLAY_PRESENT => present_objects
            .get(route.payload as usize)
            .map_or(LocalityTag::Resident, |_| LocalityTag::OverlayPresent),
        _ => LocalityTag::Resident,
    }
}

fn scan_split_regions(
    routes: &[SplitRoute],
    present_objects: &[ObjectRef<ObjectDomain>],
    rows: usize,
) -> (u64, AccessFacts) {
    let mut cursor = 0;
    let mut output = 0_u64;
    let mut facts = AccessFacts {
        cache_lines: line_count(size_of_val(routes)) + line_count(size_of_val(present_objects)),
        ..AccessFacts::default()
    };
    for row in 0..rows {
        facts.visited_rows += 1;
        facts.branches += 1;
        let tag = match routes.get(cursor) {
            Some(route) if route.row == row as u32 => {
                cursor += 1;
                facts.branches += 1;
                tag_from_split(*route, present_objects)
            }
            _ => LocalityTag::Resident,
        };
        output = output
            .wrapping_mul(131)
            .wrapping_add((row as u64).wrapping_mul(17).wrapping_add(tag.code()));
    }
    (output, facts)
}

fn random_split_regions(routes: &[SplitRoute], rows: usize) -> AccessFacts {
    let (positions, count) = random_positions(rows);
    let mut facts = AccessFacts::default();
    for row in positions.into_iter().take(count) {
        let mut low = 0;
        let mut high = routes.len();
        while low < high {
            let middle = low + (high - low) / 2;
            facts.visited_rows += 1;
            facts.branches += 1;
            if routes[middle].row < row as u32 {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
    }
    facts.cache_lines = facts.visited_rows;
    facts
}

impl CurrentLocality {
    fn build(input: &[InputRow]) -> Self {
        let (routes, promises, overlays) = sparse_parts(input);
        Self {
            generation: generation(),
            routes: routes.into_boxed_slice(),
            promises: promises.into_boxed_slice(),
            overlays: overlays.into_boxed_slice(),
        }
    }

    fn tag(&self, route: CurrentRoute) -> LocalityTag {
        match route.tag {
            PROMISED => self
                .promises
                .get(route.payload as usize)
                .map_or(LocalityTag::Resident, |_| LocalityTag::Promised),
            OVERLAY_ABSENT => self
                .overlays
                .get(route.payload as usize)
                .map_or(LocalityTag::Resident, |remote| tag_from_remote(*remote)),
            _ => LocalityTag::Resident,
        }
    }

    fn scan(&self, rows: usize) -> (u64, AccessFacts) {
        let _shared_generation = self.generation;
        let mut cursor = 0;
        let mut output = 0_u64;
        let mut facts = AccessFacts {
            cache_lines: line_count(self.routes.len() * size_of::<CurrentRoute>())
                + line_count(self.promises.len() * size_of::<ProviderSet>())
                + line_count(self.overlays.len() * size_of::<RemoteBase<ObjectDomain>>()),
            ..AccessFacts::default()
        };
        for row in 0..rows {
            facts.visited_rows += 1;
            facts.branches += 1;
            let tag = match self.routes.get(cursor) {
                Some(route) if route.row == row as u32 => {
                    cursor += 1;
                    facts.branches += 1;
                    self.tag(*route)
                }
                _ => LocalityTag::Resident,
            };
            output = output
                .wrapping_mul(131)
                .wrapping_add((row as u64).wrapping_mul(17).wrapping_add(tag.code()));
        }
        (output, facts)
    }

    fn random(&self, rows: usize) -> AccessFacts {
        let (positions, count) = random_positions(rows);
        let mut facts = AccessFacts::default();
        for row in positions.into_iter().take(count) {
            let mut low = 0;
            let mut high = self.routes.len();
            while low < high {
                let middle = low + (high - low) / 2;
                facts.visited_rows += 1;
                facts.branches += 1;
                if self.routes[middle].row < row as u32 {
                    low = middle + 1;
                } else {
                    high = middle;
                }
            }
            if self
                .routes
                .get(low)
                .is_some_and(|route| route.row == row as u32)
            {
                facts.branches += 1;
            }
        }
        facts.cache_lines = facts.visited_rows;
        facts
    }

    fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.routes.len() * size_of::<CurrentRoute>()
            + self.promises.len() * size_of::<ProviderSet>()
            + self.overlays.len() * size_of::<RemoteBase<ObjectDomain>>()
    }
}

impl FusedLocality {
    fn build(input: &[InputRow]) -> Self {
        let shared_generation = generation();
        let rows = input
            .iter()
            .map(|input| match input.tag {
                LocalityTag::Resident => FusedRow::Resident,
                LocalityTag::Promised => FusedRow::Promised(input.provider),
                LocalityTag::OverlayAbsent => FusedRow::Overlay(RemoteBase::Absent {
                    generation: shared_generation,
                }),
                LocalityTag::OverlayPresent => FusedRow::Overlay(RemoteBase::Present {
                    generation: shared_generation,
                    object: input.object,
                }),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            generation: shared_generation,
            rows,
        }
    }

    fn scan(&self) -> (u64, AccessFacts) {
        let _shared_generation = self.generation;
        let facts = AccessFacts {
            visited_rows: self.rows.len(),
            branches: self.rows.len(),
            atomics: 0,
            cache_lines: line_count(self.rows.len() * size_of::<FusedRow>()),
        };
        let output = checksum(self.rows.iter().map(|row| match row {
            FusedRow::Resident => LocalityTag::Resident,
            FusedRow::Promised(provider) => {
                let _provider = provider;
                LocalityTag::Promised
            }
            FusedRow::Overlay(remote) => tag_from_remote(*remote),
        }));
        (output, facts)
    }

    fn random(&self, rows: usize) -> AccessFacts {
        let (_, count) = random_positions(rows);
        AccessFacts {
            visited_rows: count,
            branches: count,
            atomics: 0,
            cache_lines: count,
        }
    }

    fn retained_bytes(&self) -> usize {
        size_of::<Self>() + self.rows.len() * size_of::<FusedRow>()
    }
}

impl SplitLocality {
    fn build(input: &[InputRow]) -> Self {
        let (routes, present_objects) = split_parts(input);
        Self {
            generation: generation(),
            routes: routes.into_boxed_slice(),
            present_objects: present_objects.into_boxed_slice(),
        }
    }

    fn scan(&self, rows: usize) -> (u64, AccessFacts) {
        scan_split_regions(&self.routes, &self.present_objects, rows)
    }

    fn random(&self, rows: usize) -> AccessFacts {
        random_split_regions(&self.routes, rows)
    }

    fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.routes.len() * size_of::<SplitRoute>()
            + self.present_objects.len() * size_of::<ObjectRef<ObjectDomain>>()
    }
}

impl<'header, 'routes, 'objects> LeasedRegionsView<'header, 'routes, 'objects> {
    fn scan(&self, rows: usize) -> (u64, AccessFacts) {
        let _shared_generation = self.generation;
        scan_split_regions(self.routes, self.present_objects, rows)
    }

    fn random(&self, rows: usize) -> AccessFacts {
        random_split_regions(self.routes, rows)
    }
}

impl Packed13Locality {
    fn build(input: &[InputRow]) -> Self {
        let (routes, present_objects) = split_parts(input);
        let routes = routes
            .into_iter()
            .map(|route| PackedRoute13 {
                row: route.row,
                tag: route.tag,
                payload: route.payload,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            generation: generation(),
            routes,
            present_objects: present_objects.into_boxed_slice(),
        }
    }

    fn tag(&self, route: PackedRoute13) -> LocalityTag {
        match route.tag() {
            PROMISED => LocalityTag::Promised,
            OVERLAY_ABSENT => LocalityTag::OverlayAbsent,
            OVERLAY_PRESENT => self
                .present_objects
                .get(route.payload() as usize)
                .map_or(LocalityTag::Resident, |_| LocalityTag::OverlayPresent),
            _ => LocalityTag::Resident,
        }
    }

    fn scan(&self, rows: usize) -> (u64, AccessFacts) {
        let mut cursor = 0;
        let mut output = 0_u64;
        let mut facts = AccessFacts {
            cache_lines: line_count(self.routes.len() * size_of::<PackedRoute13>())
                + line_count(self.present_objects.len() * size_of::<ObjectRef<ObjectDomain>>()),
            ..AccessFacts::default()
        };
        let _generation = self.generation;
        for row in 0..rows {
            facts.visited_rows += 1;
            facts.branches += 1;
            let tag = match self.routes.get(cursor).copied() {
                Some(route) if route.row() == row as u32 => {
                    cursor += 1;
                    facts.branches += 1;
                    self.tag(route)
                }
                _ => LocalityTag::Resident,
            };
            output = output
                .wrapping_mul(131)
                .wrapping_add((row as u64).wrapping_mul(17).wrapping_add(tag.code()));
        }
        (output, facts)
    }

    fn random(&self, rows: usize) -> AccessFacts {
        let (positions, count) = random_positions(rows);
        let mut facts = AccessFacts::default();
        for row in positions.into_iter().take(count) {
            let mut low = 0;
            let mut high = self.routes.len();
            while low < high {
                let middle = low + (high - low) / 2;
                facts.visited_rows += 1;
                facts.branches += 1;
                if self.routes[middle].row() < row as u32 {
                    low = middle + 1;
                } else {
                    high = middle;
                }
            }
        }
        facts.cache_lines = facts.visited_rows;
        facts
    }

    fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.routes.len() * size_of::<PackedRoute13>()
            + self.present_objects.len() * size_of::<ObjectRef<ObjectDomain>>()
    }
}

fn align_up(value: usize, alignment: usize) -> usize {
    let remainder = value % alignment;
    if remainder == 0 {
        value
    } else {
        value + alignment - remainder
    }
}

impl PackedLocality {
    fn build(input: &[InputRow]) -> Result<Self, ()> {
        let (routes, promises, overlays) = sparse_parts(input);
        let mut cursor = 0;
        let generation_offset = align_up(cursor, align_of::<GenerationId>());
        cursor = generation_offset + size_of::<GenerationId>();
        let routes_offset = align_up(cursor, align_of::<CurrentRoute>());
        cursor = routes_offset + routes.len() * size_of::<CurrentRoute>();
        let promises_offset = align_up(cursor, align_of::<ProviderSet>());
        cursor = promises_offset + promises.len() * size_of::<ProviderSet>();
        let overlays_offset = align_up(cursor, align_of::<RemoteBase<ObjectDomain>>());
        cursor = overlays_offset + overlays.len() * size_of::<RemoteBase<ObjectDomain>>();
        let allocation =
            match Layout::from_size_align(cursor, align_of::<RemoteBase<ObjectDomain>>()) {
                Ok(allocation) => allocation,
                Err(_) => return Err(()),
            };
        // SAFETY: `allocation` has nonzero alignment and was computed from bounded model slices.
        let pointer = unsafe { std::alloc::alloc(allocation) };
        let pointer = NonNull::new(pointer).ok_or(())?;
        // SAFETY: the aligned, disjoint ranges below fit the freshly allocated block exactly. All
        // copied types are `Copy`, so bytewise initialization creates valid values with no drops.
        unsafe {
            pointer
                .as_ptr()
                .add(generation_offset)
                .cast::<GenerationId>()
                .write(generation());
            core::ptr::copy_nonoverlapping(
                routes.as_ptr(),
                pointer.as_ptr().add(routes_offset).cast::<CurrentRoute>(),
                routes.len(),
            );
            core::ptr::copy_nonoverlapping(
                promises.as_ptr(),
                pointer.as_ptr().add(promises_offset).cast::<ProviderSet>(),
                promises.len(),
            );
            core::ptr::copy_nonoverlapping(
                overlays.as_ptr(),
                pointer
                    .as_ptr()
                    .add(overlays_offset)
                    .cast::<RemoteBase<ObjectDomain>>(),
                overlays.len(),
            );
        }
        let payload_bytes = size_of::<GenerationId>()
            + routes.len() * size_of::<CurrentRoute>()
            + promises.len() * size_of::<ProviderSet>()
            + overlays.len() * size_of::<RemoteBase<ObjectDomain>>();
        Ok(Self {
            pointer,
            allocation,
            generation_offset,
            routes_offset,
            routes_len: routes.len(),
            promises_offset,
            promises_len: promises.len(),
            overlays_offset,
            overlays_len: overlays.len(),
            padding_bytes: cursor - payload_bytes,
        })
    }

    fn generation(&self) -> &GenerationId {
        // SAFETY: `build` wrote one `GenerationId` at this aligned in-allocation offset, and the
        // owner outlives the returned borrow. No mutable alias is ever exposed.
        unsafe {
            &*self
                .pointer
                .as_ptr()
                .add(self.generation_offset)
                .cast::<GenerationId>()
        }
    }

    fn routes(&self) -> &[CurrentRoute] {
        // SAFETY: `build` initialized exactly `routes_len` copies at the stored aligned offset;
        // this owner never reallocates or mutates its allocation after construction.
        unsafe {
            core::slice::from_raw_parts(
                self.pointer
                    .as_ptr()
                    .add(self.routes_offset)
                    .cast::<CurrentRoute>(),
                self.routes_len,
            )
        }
    }

    fn promises(&self) -> &[ProviderSet] {
        // SAFETY: same construction/alignment/lifetime proof as `routes` for the provider segment.
        unsafe {
            core::slice::from_raw_parts(
                self.pointer
                    .as_ptr()
                    .add(self.promises_offset)
                    .cast::<ProviderSet>(),
                self.promises_len,
            )
        }
    }

    fn overlays(&self) -> &[RemoteBase<ObjectDomain>] {
        // SAFETY: same construction/alignment/lifetime proof as `routes` for the overlay segment.
        unsafe {
            core::slice::from_raw_parts(
                self.pointer
                    .as_ptr()
                    .add(self.overlays_offset)
                    .cast::<RemoteBase<ObjectDomain>>(),
                self.overlays_len,
            )
        }
    }

    fn tag(&self, route: CurrentRoute) -> LocalityTag {
        match route.tag {
            PROMISED => self
                .promises()
                .get(route.payload as usize)
                .map_or(LocalityTag::Resident, |_| LocalityTag::Promised),
            OVERLAY_ABSENT => self
                .overlays()
                .get(route.payload as usize)
                .map_or(LocalityTag::Resident, |remote| tag_from_remote(*remote)),
            _ => LocalityTag::Resident,
        }
    }

    fn scan(&self, rows: usize) -> (u64, AccessFacts) {
        let routes = self.routes();
        let mut cursor = 0;
        let mut output = 0_u64;
        let mut facts = AccessFacts {
            cache_lines: line_count(size_of_val(routes))
                + line_count(size_of_val(self.promises()))
                + line_count(size_of_val(self.overlays())),
            ..AccessFacts::default()
        };
        let _generation = self.generation();
        for row in 0..rows {
            facts.visited_rows += 1;
            facts.branches += 1;
            let tag = match routes.get(cursor) {
                Some(route) if route.row == row as u32 => {
                    cursor += 1;
                    facts.branches += 1;
                    self.tag(*route)
                }
                _ => LocalityTag::Resident,
            };
            output = output
                .wrapping_mul(131)
                .wrapping_add((row as u64).wrapping_mul(17).wrapping_add(tag.code()));
        }
        (output, facts)
    }

    fn random(&self, rows: usize) -> AccessFacts {
        let routes = self.routes();
        let (positions, count) = random_positions(rows);
        let mut facts = AccessFacts::default();
        for row in positions.into_iter().take(count) {
            let mut low = 0;
            let mut high = routes.len();
            while low < high {
                let middle = low + (high - low) / 2;
                facts.visited_rows += 1;
                facts.branches += 1;
                if routes[middle].row < row as u32 {
                    low = middle + 1;
                } else {
                    high = middle;
                }
            }
        }
        facts.cache_lines = facts.visited_rows;
        facts
    }

    fn retained_bytes(&self) -> usize {
        size_of::<Self>() + self.allocation.size()
    }
}

impl Drop for PackedLocality {
    fn drop(&mut self) {
        // SAFETY: `build` allocated this exact non-null pointer/layout pair, values are all `Copy`,
        // and this unique owner exposes only shared borrows before its single deallocation.
        unsafe { std::alloc::dealloc(self.pointer.as_ptr(), self.allocation) };
    }
}

impl<'input> CallerView<'input> {
    fn scan(&self) -> (u64, AccessFacts) {
        (
            checksum(self.tags.iter().copied()),
            AccessFacts {
                visited_rows: self.tags.len(),
                branches: self.tags.len(),
                atomics: 0,
                cache_lines: line_count(size_of_val(self.tags)),
            },
        )
    }

    fn random(&self) -> AccessFacts {
        let (_, count) = random_positions(self.tags.len());
        AccessFacts {
            visited_rows: count,
            branches: count,
            atomics: 0,
            cache_lines: count,
        }
    }
}

fn remote_and_generation_bytes(input: &[InputRow]) -> (usize, usize) {
    let overlays = input
        .iter()
        .filter(|input| {
            matches!(
                input.tag,
                LocalityTag::OverlayAbsent | LocalityTag::OverlayPresent
            )
        })
        .count();
    (
        overlays * size_of::<RemoteBase<ObjectDomain>>(),
        overlays.saturating_sub(1) * size_of::<GenerationId>(),
    )
}

fn current_facts(input: &[InputRow], expected: u64) -> LocalityFacts {
    let model = CurrentLocality::build(input);
    let (output, scan) = model.scan(input.len());
    let random = model.random(input.len());
    let (remote_base_bytes, repeated_generation_bytes) = remote_and_generation_bytes(input);
    LocalityFacts {
        representation_bytes: model.retained_bytes(),
        caller_bytes: 0,
        padding_bytes: 0,
        remote_base_bytes,
        repeated_generation_bytes,
        scan,
        random,
        exact: output == expected,
    }
}

fn fused_facts(input: &[InputRow], expected: u64) -> LocalityFacts {
    let model = FusedLocality::build(input);
    let (output, scan) = model.scan();
    let random = model.random(input.len());
    let (remote_base_bytes, repeated_generation_bytes) = remote_and_generation_bytes(input);
    LocalityFacts {
        representation_bytes: model.retained_bytes(),
        caller_bytes: 0,
        padding_bytes: 0,
        remote_base_bytes,
        repeated_generation_bytes,
        scan,
        random,
        exact: output == expected,
    }
}

fn split_facts(input: &[InputRow], expected: u64) -> LocalityFacts {
    let model = SplitLocality::build(input);
    let (output, scan) = model.scan(input.len());
    let random = model.random(input.len());
    let overlays = input
        .iter()
        .filter(|input| {
            matches!(
                input.tag,
                LocalityTag::OverlayAbsent | LocalityTag::OverlayPresent
            )
        })
        .count();
    LocalityFacts {
        representation_bytes: model.retained_bytes(),
        caller_bytes: 0,
        padding_bytes: 0,
        remote_base_bytes: 0,
        repeated_generation_bytes: 0,
        scan,
        random,
        exact: output == expected && (overlays == 0 || model.generation == generation()),
    }
}

fn packed13_facts(input: &[InputRow], expected: u64) -> LocalityFacts {
    let model = Packed13Locality::build(input);
    let (output, scan) = model.scan(input.len());
    let random = model.random(input.len());
    LocalityFacts {
        representation_bytes: model.retained_bytes(),
        caller_bytes: 0,
        padding_bytes: 0,
        remote_base_bytes: 0,
        repeated_generation_bytes: 0,
        scan,
        random,
        exact: output == expected && model.generation == generation(),
    }
}

fn packed_facts(input: &[InputRow], expected: u64) -> Result<LocalityFacts, ()> {
    let model = PackedLocality::build(input)?;
    let (output, scan) = model.scan(input.len());
    let random = model.random(input.len());
    let (remote_base_bytes, repeated_generation_bytes) = remote_and_generation_bytes(input);
    Ok(LocalityFacts {
        representation_bytes: model.retained_bytes(),
        caller_bytes: 0,
        padding_bytes: model.padding_bytes,
        remote_base_bytes,
        repeated_generation_bytes,
        scan,
        random,
        exact: output == expected,
    })
}

fn caller_facts(input: &[InputRow], expected: u64) -> LocalityFacts {
    let tags = input.iter().map(|input| input.tag).collect::<Vec<_>>();
    let view = CallerView { tags: &tags };
    let (output, scan) = view.scan();
    let random = view.random();
    LocalityFacts {
        representation_bytes: size_of::<CallerView<'_>>(),
        caller_bytes: size_of::<Vec<LocalityTag>>() + tags.capacity() * size_of::<LocalityTag>(),
        padding_bytes: 0,
        remote_base_bytes: 0,
        repeated_generation_bytes: 0,
        scan,
        random,
        exact: output == expected,
    }
}

fn leased_regions_facts(input: &[InputRow], expected: u64) -> LocalityFacts {
    let (routes, present_objects) = split_parts(input);
    let shared_generation = generation();
    let view = LeasedRegionsView {
        generation: &shared_generation,
        routes: &routes,
        present_objects: &present_objects,
    };
    let (output, scan) = view.scan(input.len());
    let random = view.random(input.len());
    LocalityFacts {
        representation_bytes: size_of::<LeasedRegionsView<'_, '_, '_>>(),
        caller_bytes: size_of::<GenerationId>()
            + size_of::<Vec<SplitRoute>>()
            + routes.capacity() * size_of::<SplitRoute>()
            + size_of::<Vec<ObjectRef<ObjectDomain>>>()
            + present_objects.capacity() * size_of::<ObjectRef<ObjectDomain>>(),
        padding_bytes: 0,
        remote_base_bytes: 0,
        repeated_generation_bytes: 0,
        scan,
        random,
        exact: output == expected,
    }
}

fn print_row(
    representation: &str,
    rows: usize,
    profile: LocalityProfile,
    facts: LocalityFacts,
    allocation: AllocationFacts,
) {
    println!(
        "locality\t{representation}\t{rows}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        profile.name(),
        bool_count(facts.exact),
        facts.representation_bytes,
        facts.caller_bytes,
        facts.representation_bytes + facts.caller_bytes,
        facts.padding_bytes,
        facts.remote_base_bytes,
        facts.repeated_generation_bytes,
        facts.scan.visited_rows,
        facts.scan.branches,
        facts.scan.cache_lines,
        facts.random.visited_rows,
        facts.random.branches,
        facts.random.cache_lines,
        allocation.allocations,
        allocation.allocated_bytes,
        allocation.peak_live_bytes,
        allocation.deallocations,
    );
}

pub(super) fn rows(allocator: &TrackingAllocator) {
    println!(
        "columns\tlocality\trepresentation\trows\tprofile\texact_output\trepresentation_retained_bytes\tcaller_retained_bytes\ttotal_retained_bytes\tpacked_padding_bytes\tremote_base_bytes\trepeated_generation_bytes\tscan_rows\tscan_branches\tscan_logical_cache_lines\trandom_steps\trandom_branches\trandom_logical_cache_lines\tallocations\tallocated_bytes\tpeak_live_bytes\tdrop_deallocations"
    );
    println!(
        "locality_type\tRemoteBase\tbytes\t{}\tGenerationId\tbytes\t{}\tFusedRow\tbytes\t{}\tCurrentRoute\tbytes\t{}\tSplitRoute16\tbytes\t{}\tPackedRoute13\tbytes\t{}",
        size_of::<RemoteBase<ObjectDomain>>(),
        size_of::<GenerationId>(),
        size_of::<FusedRow>(),
        size_of::<CurrentRoute>(),
        size_of::<SplitRoute>(),
        size_of::<PackedRoute13>(),
    );
    for rows in [0_usize, 1, 100_000] {
        for profile in [
            LocalityProfile::Promised,
            LocalityProfile::OverlayAbsent,
            LocalityProfile::OverlayPresent,
            LocalityProfile::Mixed,
        ] {
            let input = match input(rows, profile) {
                Ok(input) => input,
                Err(error) => {
                    println!("locality_setup_error\t{rows}\t{}\t{error}", profile.name());
                    continue;
                }
            };
            let expected = expected_checksum(&input);
            let (facts, allocation) = allocator.measure(|| current_facts(&input, expected));
            print_row("current_three_box_slices", rows, profile, facts, allocation);
            let (facts, allocation) = allocator.measure(|| fused_facts(&input, expected));
            print_row("fused_enum_per_row", rows, profile, facts, allocation);
            let (facts, allocation) = allocator.measure(|| split_facts(&input, expected));
            print_row(
                "split_16B_route_shared_generation",
                rows,
                profile,
                facts,
                allocation,
            );
            let (facts, allocation) = allocator.measure(|| packed13_facts(&input, expected));
            print_row(
                "packed_13B_route_shared_generation",
                rows,
                profile,
                facts,
                allocation,
            );
            let (packed, allocation) = allocator.measure(|| packed_facts(&input, expected));
            match packed {
                Ok(facts) => print_row(
                    "unsafe_packed_single_owner",
                    rows,
                    profile,
                    facts,
                    allocation,
                ),
                Err(()) => println!(
                    "locality_build_error\tunsafe_packed_single_owner\t{rows}\t{}\tallocation_failed",
                    profile.name()
                ),
            }
            let (facts, allocation) = allocator.measure(|| caller_facts(&input, expected));
            print_row("caller_owned_output_view", rows, profile, facts, allocation);
            let (facts, allocation) = allocator.measure(|| leased_regions_facts(&input, expected));
            print_row(
                "borrowed_separately_leased_regions",
                rows,
                profile,
                facts,
                allocation,
            );
        }
    }
    dense_exception_rank_rows(allocator);
    production_rank_rows(allocator);
    view_header_rows();
}

fn text_input() -> Vec<InputRow> {
    input(64, LocalityProfile::Mixed).unwrap_or_default()
}

pub(super) fn text_current() -> usize {
    let input = text_input();
    let model = CurrentLocality::build(&input);
    let (output, access) = model.scan(input.len());
    black_box(output ^ access.visited_rows as u64) as usize
}

pub(super) fn text_fused() -> usize {
    let input = text_input();
    let model = FusedLocality::build(&input);
    let (output, access) = model.scan();
    black_box(output ^ access.visited_rows as u64) as usize
}

pub(super) fn text_split() -> usize {
    let input = text_input();
    let model = SplitLocality::build(&input);
    let (output, access) = model.scan(input.len());
    black_box(output ^ access.visited_rows as u64) as usize
}

pub(super) fn text_packed() -> usize {
    let input = text_input();
    match PackedLocality::build(&input) {
        Ok(model) => {
            let (output, access) = model.scan(input.len());
            black_box(output ^ access.visited_rows as u64) as usize
        }
        Err(()) => 0,
    }
}

pub(super) fn text_packed13() -> usize {
    let input = text_input();
    let model = Packed13Locality::build(&input);
    let (output, access) = model.scan(input.len());
    black_box(output ^ access.visited_rows as u64) as usize
}

#[cfg(test)]
#[test]
fn every_locality_candidate_is_exact_at_all_profiles() -> Result<(), ()> {
    for rows in [0_usize, 1, 100_000] {
        for profile in [
            LocalityProfile::Promised,
            LocalityProfile::OverlayAbsent,
            LocalityProfile::OverlayPresent,
            LocalityProfile::Mixed,
        ] {
            let input = match input(rows, profile) {
                Ok(input) => input,
                Err(_) => return Err(()),
            };
            let expected = expected_checksum(&input);
            assert!(current_facts(&input, expected).exact);
            assert!(fused_facts(&input, expected).exact);
            assert!(split_facts(&input, expected).exact);
            assert!(packed13_facts(&input, expected).exact);
            let packed = match packed_facts(&input, expected) {
                Ok(facts) => facts,
                Err(()) => return Err(()),
            };
            assert!(packed.exact);
            assert!(caller_facts(&input, expected).exact);
            assert!(leased_regions_facts(&input, expected).exact);
        }
    }
    for roots in [0_usize, 1, 100_000] {
        let densities: &[RankDensity] = match roots {
            0 => &[RankDensity::Empty],
            1 => &[RankDensity::Empty, RankDensity::Full],
            _ => &[
                RankDensity::Empty,
                RankDensity::One,
                RankDensity::OnePercent,
                RankDensity::FivePercent,
                RankDensity::TwentyFivePercent,
                RankDensity::FiftyPercent,
                RankDensity::Full,
            ],
        };
        for density in densities {
            let rows = exception_rows(roots, density.exceptions(roots));
            let index = Prefix256Rank::build(roots, &rows);
            let evidence = rank_evidence(&index, roots, &rows);
            assert!(evidence.exact);
            let index = WordRank::build(roots, &rows);
            let evidence = rank_evidence(&index, roots, &rows);
            assert!(evidence.exact);
            let index = Rank9::build(roots, &rows);
            let evidence = rank_evidence(&index, roots, &rows);
            assert!(evidence.exact);
            let index = Prefix2048Rank::build(roots, &rows);
            let evidence = rank_evidence(&index, roots, &rows);
            assert!(evidence.exact);
            assert!(production_rank_is_exact(
                roots,
                *density,
                Prefix256Rank::build,
            ));
            assert!(production_rank_is_exact(roots, *density, WordRank::build,));
            assert!(production_rank_is_exact(roots, *density, Rank9::build,));
            assert!(production_rank_is_exact(
                roots,
                *density,
                Prefix2048Rank::build,
            ));
        }
    }
    Ok(())
}
