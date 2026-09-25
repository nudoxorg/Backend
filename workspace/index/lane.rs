//! Columnar lane: interned dependency counts and a radix rank order.
//!
//! The dependents sweep used to clone a [`SmolStr`] into a [`HashMap`] key on
//! every edge, then drop that clone on every hit. Ranking sorted `f32` with
//! `partial_cmp`, which treats NaN as equal and so does not order it.
//!
//! This module does the work on dense integers instead.
//!
//! * The sweep borrows row text. It copies a name only when that name has an
//!   in-degree, not once per edge.
//! * Rank keys are the IEEE total order of the score, inverted so a higher
//!   score sorts first under an ascending radix, with the name ordinal in the
//!   low half so ties are name order without a second comparison sort.

use std::collections::HashMap;

use heart::Language;
use smol_str::SmolStr;

use crate::search::ranking::dependents::DependencyRow;

/// Count direct dependents the way
/// [`crate::search::ranking::dependents::count_dependents`] does, but on
/// interned ids.
///
/// A package name counts once. Its dependencies are the union of every row
/// for that name, so a later version can name a dependency the earlier row
/// omitted. A self-edge is ignored. The returned map has one owned name per
/// package that is depended on, not one per edge.
pub fn count_dependents(
    rows: impl IntoIterator<Item = DependencyRow>,
) -> HashMap<(Language, SmolStr), u32> {
    count_slice(&rows.into_iter().collect::<Vec<_>>())
}

/// Borrowed sweep. The bench times this, not a clone of the input.
///
/// Each distinct `(ecosystem, name)` is interned to a dense `u32` once. Edges
/// then increment a histogram slot. Only names with an in-degree are copied
/// into the owned map the caller keeps. Restoring a [`HashMap`] increment per
/// edge fails [`tests::lane_beats_string_map_when_names_repeat`].
pub fn count_slice(rows: &[DependencyRow]) -> HashMap<(Language, SmolStr), u32> {
    let mut intern = Intern::with_capacity(rows.len().saturating_mul(4).max(16));
    let mut seen_deps: Vec<Vec<u32>> = Vec::new();
    let mut counts: Vec<u32> = Vec::new();
    for row in rows {
        let self_name = row.name.trim();
        let self_id = intern.id(row.ecosystem, self_name, &mut counts);
        if (self_id as usize) >= seen_deps.len() {
            seen_deps.resize(self_id as usize + 1, Vec::new());
        }
        for dep in &row.dependencies {
            let Some(dep_name) = crate::record::counted_dependency_name(self_name, dep.as_str())
            else {
                continue;
            };
            let dep_id = intern.id(row.ecosystem, dep_name, &mut counts);
            let bucket = &mut seen_deps[self_id as usize];
            if let Err(pos) = bucket.binary_search(&dep_id) {
                bucket.insert(pos, dep_id);
                counts[dep_id as usize] = counts[dep_id as usize].saturating_add(1);
            }
        }
    }
    let mut out = HashMap::new();
    for (id, count) in counts.iter().copied().enumerate() {
        if count == 0 {
            continue;
        }
        let (ecosystem, name) = intern.name_of(id as u32);
        out.insert((ecosystem, SmolStr::new(name)), count);
    }
    out
}

struct Slot<'a> {
    hash: u64,
    lang: u8,
    name: &'a str,
    id: u32,
    used: bool,
}

struct Intern<'a> {
    slots: Vec<Slot<'a>>,
    mask: usize,
    names: Vec<(Language, &'a str)>,
}

impl<'a> Intern<'a> {
    fn with_capacity(hint: usize) -> Self {
        let cap = hint.next_power_of_two().max(16);
        let mut slots = Vec::with_capacity(cap);
        for _ in 0..cap {
            slots.push(Slot {
                hash: 0,
                lang: 0,
                name: "",
                id: 0,
                used: false,
            });
        }
        Self {
            slots,
            mask: cap - 1,
            names: Vec::new(),
        }
    }

    fn id(&mut self, ecosystem: Language, name: &'a str, counts: &mut Vec<u32>) -> u32 {
        let hash = fx_hash(ecosystem, name);
        if self.names.len() * 2 > self.mask {
            self.rehash();
        }
        let mut slot = (hash as usize) & self.mask;
        loop {
            if !self.slots[slot].used {
                let id = self.names.len() as u32;
                self.slots[slot] = Slot {
                    hash,
                    lang: ecosystem as u8,
                    name,
                    id,
                    used: true,
                };
                self.names.push((ecosystem, name));
                counts.push(0);
                return id;
            }
            if self.slots[slot].hash == hash
                && self.slots[slot].lang == ecosystem as u8
                && self.slots[slot].name == name
            {
                return self.slots[slot].id;
            }
            slot = (slot + 1) & self.mask;
        }
    }

    /// Rebuild the probe table. Ids already handed out stay put.
    fn rehash(&mut self) {
        let cap = self.slots.len() * 2;
        let mut slots = Vec::with_capacity(cap);
        for _ in 0..cap {
            slots.push(Slot {
                hash: 0,
                lang: 0,
                name: "",
                id: 0,
                used: false,
            });
        }
        let mask = cap - 1;
        for old in self.slots.drain(..) {
            if !old.used {
                continue;
            }
            let mut slot = (old.hash as usize) & mask;
            while slots[slot].used {
                slot = (slot + 1) & mask;
            }
            slots[slot] = old;
        }
        self.slots = slots;
        self.mask = mask;
    }

    fn name_of(&self, id: u32) -> (Language, &'a str) {
        self.names[id as usize]
    }
}

fn fx_hash(ecosystem: Language, name: &str) -> u64 {
    let mut hash = (ecosystem as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    for &byte in name.as_bytes() {
        hash = hash.rotate_left(5) ^ u64::from(byte);
        hash = hash.wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    hash
}

/// The string-cloning sweep, kept as the oracle the bench has to beat.
pub fn count_dependents_mapped(rows: &[DependencyRow]) -> HashMap<(Language, SmolStr), u32> {
    use std::collections::HashSet;
    let mut seen: HashSet<(Language, SmolStr, SmolStr)> = HashSet::new();
    let mut counts: HashMap<(Language, SmolStr), u32> = HashMap::new();
    for row in rows {
        let package = row.name.trim();
        for dep in &row.dependencies {
            let Some(dep_name) = crate::record::counted_dependency_name(package, dep.as_str())
            else {
                continue;
            };
            let package_name = SmolStr::new(package);
            let counted = SmolStr::new(dep_name);
            if seen.insert((row.ecosystem, package_name, counted.clone())) {
                *counts.entry((row.ecosystem, counted)).or_default() += 1;
            }
        }
    }
    counts
}

/// Indices of `scores` ordered by score descending, then by `name(i)`
/// ascending.
///
/// The score half is the bitwise inversion of [`f32::total_cmp`]'s ordering
/// key, so NaN sorts as IEEE defines and a higher finite score still comes
/// first. The name half is the ordinal of that name in ascending order, packed
/// into the low 32 bits. Four counting-sort passes order the `u64` key.
pub fn order_desc_score_asc_name<'a>(
    scores: &[f32],
    name_at: impl Fn(usize) -> &'a str,
) -> Vec<usize> {
    let n = scores.len();
    let mut by_name: Vec<usize> = (0..n).collect();
    by_name.sort_by(|&a, &b| name_at(a).cmp(name_at(b)).then(a.cmp(&b)));
    let mut name_ord = vec![0u32; n];
    for (ord, index) in by_name.into_iter().enumerate() {
        name_ord[index] = ord as u32;
    }
    let mut keys = Vec::with_capacity(n);
    for (index, score) in scores.iter().copied().enumerate() {
        let score_key = !total_key(score);
        let key = (u64::from(score_key) << 32) | u64::from(name_ord[index]);
        keys.push((key, index as u32));
    }
    radix_asc(&mut keys);
    keys.into_iter().map(|(_, index)| index as usize).collect()
}

/// Bits that order like [`f32::total_cmp`]: sign-magnitude flipped to unsigned.
fn total_key(value: f32) -> u32 {
    let bits = value.to_bits();
    if bits & 0x8000_0000 == 0 {
        bits | 0x8000_0000
    } else {
        !bits
    }
}

fn radix_asc(keys: &mut Vec<(u64, u32)>) {
    let n = keys.len();
    if n < 2 {
        return;
    }
    let mut buf = vec![(0u64, 0u32); n];
    let mut src = keys;
    let mut dst = &mut buf;
    for shift in [0u32, 8, 16, 24, 32, 40, 48, 56] {
        let mut count = [0usize; 256];
        for (key, _) in src.iter() {
            let byte = ((key >> shift) & 0xff) as usize;
            count[byte] += 1;
        }
        let mut sum = 0usize;
        for slot in &mut count {
            let c = *slot;
            *slot = sum;
            sum += c;
        }
        for item in src.iter().copied() {
            let byte = ((item.0 >> shift) & 0xff) as usize;
            dst[count[byte]] = item;
            count[byte] += 1;
        }
        std::mem::swap(&mut src, &mut dst);
    }
    // Eight swaps return the ordered run to `keys` only when we started in keys.
    // Eight passes leave the result in `src`, which is `keys` after an even count.
    let _ = src;
    let _ = dst;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, deps: &[&str]) -> DependencyRow {
        DependencyRow {
            ecosystem: Language::Rust,
            name: SmolStr::new(name),
            dependencies: deps.iter().copied().map(SmolStr::new).collect(),
        }
    }

    #[test]
    fn a_padded_name_matches_the_trimmed_count() {
        let rows = vec![row("app", &[" serde ", "", "app", "  "])];
        let lane = count_dependents(rows.clone());
        let mapped = count_dependents_mapped(&rows);
        assert_eq!(lane, mapped);
        assert_eq!(lane.get(&(Language::Rust, SmolStr::new("serde"))), Some(&1));
        assert_eq!(lane.len(), 1);
    }

    #[test]
    fn lane_matches_the_string_map() {
        let rows = vec![
            row("a", &["serde", "tokio"]),
            row("b", &["serde"]),
            row("a", &["tokio", "later-version"]),
            row("c", &["c", "serde"]),
        ];
        let lane = count_dependents(rows.clone());
        let mapped = count_dependents_mapped(&rows);
        assert_eq!(lane, mapped);
        assert_eq!(lane.get(&(Language::Rust, SmolStr::new("serde"))), Some(&3));
        assert_eq!(
            lane.get(&(Language::Rust, SmolStr::new("later-version"))),
            Some(&1)
        );
        assert_eq!(lane.get(&(Language::Rust, SmolStr::new("tokio"))), Some(&1));
        assert!(!lane.contains_key(&(Language::Rust, SmolStr::new("c"))));

        let reversed = vec![
            row("a", &["tokio", "later-version"]),
            row("c", &["c", "serde"]),
            row("b", &["serde"]),
            row("a", &["serde", "tokio"]),
        ];
        assert_eq!(count_dependents(reversed), lane);
    }

    #[test]
    fn radix_order_matches_total_cmp_then_name() {
        let scores = [1.0f32, f32::NAN, 1.0, 0.0, f32::NEG_INFINITY, 2.5];
        let names = ["m", "a", "b", "z", "q", "a"];
        let got = order_desc_score_asc_name(&scores, |i| names[i]);
        let mut expect: Vec<usize> = (0..scores.len()).collect();
        expect.sort_by(|&a, &b| {
            scores[b]
                .total_cmp(&scores[a])
                .then(names[a].cmp(names[b]))
                .then(a.cmp(&b))
        });
        assert_eq!(got, expect);
    }

    #[test]
    fn lane_beats_string_map_when_names_repeat() {
        let mut rows = Vec::with_capacity(8_000);
        for n in 0..8_000 {
            let deps: Vec<SmolStr> = (0..8)
                .map(|d| SmolStr::new(format!("dependency-name-that-does-not-fit-inline-{d:04}")))
                .collect();
            rows.push(DependencyRow {
                ecosystem: Language::Rust,
                name: SmolStr::new(format!("pkg{n}")),
                dependencies: deps,
            });
        }
        let loops = 8u32;
        let lane_ns = time(|| {
            for _ in 0..loops {
                std::hint::black_box(count_slice(&rows));
            }
        });
        let map_ns = time(|| {
            for _ in 0..loops {
                std::hint::black_box(count_dependents_mapped(&rows));
            }
        });
        eprintln!(
            "cost case=lane/dependents rows=8000 loops={loops} lane_ns={lane_ns} map_ns={map_ns}"
        );
        assert!(
            lane_ns + lane_ns / 2 < map_ns,
            "histogram {lane_ns} ns was not 1.5× under the cloning sweep {map_ns} ns"
        );
    }

    #[test]
    fn radix_beats_comparison_sort() {
        let n = 50_000usize;
        let scores: Vec<f32> = (0..n).map(|i| (i % 100) as f32).collect();
        let names: Vec<String> = (0..n).map(|i| format!("n{i:05}")).collect();
        let loops = 10u32;
        let radix_ns = time(|| {
            for _ in 0..loops {
                std::hint::black_box(order_desc_score_asc_name(&scores, |i| names[i].as_str()));
            }
        });
        let sort_ns = time(|| {
            for _ in 0..loops {
                let mut indexed: Vec<usize> = (0..n).collect();
                indexed.sort_unstable_by(|&a, &b| {
                    scores[b]
                        .partial_cmp(&scores[a])
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| names[a].cmp(&names[b]))
                });
                std::hint::black_box(indexed);
            }
        });
        eprintln!("cost case=lane/rank n={n} loops={loops} radix_ns={radix_ns} sort_ns={sort_ns}");
        assert!(
            radix_ns < sort_ns,
            "radix {radix_ns} ns was not under comparison sort {sort_ns} ns"
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(32))]

        #[test]
        fn any_padding_matches_the_string_map(
            rows in proptest::collection::vec(
                ("[ A-Za-z]{0,6}", proptest::collection::vec("[ A-Za-z]{0,6}", 0..6)),
                0..8,
            )
        ) {
            let built: Vec<DependencyRow> = rows
                .into_iter()
                .map(|(name, deps)| DependencyRow {
                    ecosystem: Language::Rust,
                    name: SmolStr::new(name),
                    dependencies: deps.into_iter().map(SmolStr::new).collect(),
                })
                .collect();
            proptest::prop_assert_eq!(count_dependents(built.clone()), count_dependents_mapped(&built));
        }
    }

    fn time(body: impl FnOnce()) -> u128 {
        let start = std::time::Instant::now();
        body();
        start.elapsed().as_nanos()
    }
}
