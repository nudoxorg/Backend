//! Project IR entries through [`content_delta`](crate::delta::content_delta).
//!
//! An intro id whose content hash is unchanged is omitted from every set. Same
//! hash means skip: reordering an entry is not a root rewrite.

use crate::delta::Delta;

/// One IR entry addressed by intro id and the hash of the payload a sink would
/// write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrEntryKey {
    /// Stable intro identity. Encoded as lowercase hex for
    /// [`ContentKey::id`](crate::delta::ContentKey).
    pub intro_id: [u8; 32],
    /// Content hash of the entry payload. Passed as
    /// [`ContentKey`](crate::delta::ContentKey) payload bytes.
    pub content_hash: [u8; 32],
}

/// Diff `prior` and `next` by intro id.
///
/// Unchanged hashes are absent from every set. A known intro id with a new hash
/// is [`Delta::changed`](Delta::changed), not added. An intro id missing from
/// `next` is [`Delta::removed`](Delta::removed).
///
/// This is a sort plus a two-pointer merge on the raw 32-byte intro id. It does
/// not hex-encode ids and it does not build a tree. Duplicate intro ids keep
/// the last occurrence in input order, matching the previous map.
pub fn project(prior: &[IrEntryKey], next: &[IrEntryKey]) -> Delta<IrEntryKey> {
    merge_latest(latest_by_id(prior), latest_by_id(next))
}

fn merge_latest(prior: Vec<IrEntryKey>, next: Vec<IrEntryKey>) -> Delta<IrEntryKey> {
    crate::delta::merge_sorted(
        &prior,
        &next,
        |left, right| left.intro_id.cmp(&right.intro_id),
        |left, right| left.content_hash == right.content_hash,
    )
    .into_delta()
}

/// Reference classifier: hex id into a `BTreeMap`, then [`content_delta`].
///
/// Kept so benches and the differential test can show the merge was not
/// replaced by this path. Not the function callers should use.
pub fn project_via_map(prior: &[IrEntryKey], next: &[IrEntryKey]) -> Delta<IrEntryKey> {
    use std::collections::BTreeMap;

    use smol_str::SmolStr;

    use crate::delta::{ContentKey, content_delta_via_map};

    fn hex_id(intro_id: &[u8; 32]) -> SmolStr {
        SmolStr::new(data_encoding::HEXLOWER.encode(intro_id))
    }
    fn key(entry: IrEntryKey) -> ContentKey {
        ContentKey::hash("ir-entry", hex_id(&entry.intro_id), &entry.content_hash)
    }

    let prior_keys: Vec<ContentKey> = prior.iter().copied().map(key).collect();
    let next_keys: Vec<ContentKey> = next.iter().copied().map(key).collect();
    let classified = content_delta_via_map(&prior_keys, &next_keys);
    let mut by_id = BTreeMap::<SmolStr, IrEntryKey>::new();
    for entry in prior {
        by_id.insert(hex_id(&entry.intro_id), *entry);
    }
    for entry in next {
        by_id.insert(hex_id(&entry.intro_id), *entry);
    }
    let pick = |keys: &[ContentKey]| -> Vec<IrEntryKey> {
        keys.iter()
            .filter_map(|key| by_id.get(&key.id).copied())
            .collect()
    };
    Delta {
        added: pick(&classified.added),
        changed: pick(&classified.changed),
        removed: pick(&classified.removed),
    }
}

/// One row per intro id: the last occurrence in input order, then sorted by id
/// so the merge can walk both sides once.
///
/// Order is an MSD radix on the raw id bytes ([`crate::frontier::id_radix`]).
/// A comparison sort of the same keys is [`latest_by_cmp`], kept only so the
/// bench can fail a rewrite that puts that sort back on this path.
fn latest_by_id(entries: &[IrEntryKey]) -> Vec<IrEntryKey> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut order: Vec<u32> = (0..entries.len() as u32).collect();
    crate::frontier::id_radix::sort_ids(&mut order, |index, byte| {
        entries[index as usize].intro_id[byte]
    });
    dedup_last(entries, &order)
}

/// Comparison-sort twin of [`latest_by_id`]. Same last-wins rule. Not used by
/// [`project`].
fn latest_by_cmp(entries: &[IrEntryKey]) -> Vec<IrEntryKey> {
    let mut order: Vec<u32> = (0..entries.len() as u32).collect();
    order.sort_by(|&i, &j| {
        entries[i as usize]
            .intro_id
            .cmp(&entries[j as usize].intro_id)
            .then(j.cmp(&i))
    });
    // `sort_by` with reverse index puts the last input first inside a tie.
    // Collapse by keeping the first of each id run.
    let mut out = Vec::with_capacity(entries.len());
    let mut previous: Option<[u8; 32]> = None;
    for index in order {
        let entry = entries[index as usize];
        if previous == Some(entry.intro_id) {
            continue;
        }
        previous = Some(entry.intro_id);
        out.push(entry);
    }
    out
}

fn dedup_last(entries: &[IrEntryKey], order: &[u32]) -> Vec<IrEntryKey> {
    let mut out = Vec::with_capacity(entries.len());
    let mut index = 0;
    while index < order.len() {
        let mut end = index + 1;
        let id = entries[order[index] as usize].intro_id;
        while end < order.len() && entries[order[end] as usize].intro_id == id {
            end += 1;
        }
        out.push(entries[order[end - 1] as usize]);
        index = end;
    }
    out
}

/// [`project`] with the comparison-sort preparation. The radix path must
/// classify identically and beat this on a large id set.
pub fn project_via_cmp(prior: &[IrEntryKey], next: &[IrEntryKey]) -> Delta<IrEntryKey> {
    merge_latest(latest_by_cmp(prior), latest_by_cmp(next))
}

/// Whether applying `delta` must rewrite the IR root.
///
/// False only when `delta` is empty.
pub fn needs_root_rewrite(delta: &Delta<IrEntryKey>) -> bool {
    !delta.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(intro_byte: u8, hash_byte: u8) -> IrEntryKey {
        IrEntryKey {
            intro_id: [intro_byte; 32],
            content_hash: [hash_byte; 32],
        }
    }

    #[test]
    fn unchanged_entry_is_absent_from_every_set() {
        let stayed = entry(1, 9);
        let also = entry(2, 8);
        // Same hashes, opposite order: a move is not a change.
        let delta = project(&[stayed, also], &[also, stayed]);
        assert!(delta.is_empty());
        assert!(!delta.added.contains(&stayed));
        assert!(!delta.changed.contains(&stayed));
        assert!(!delta.removed.contains(&stayed));
        assert!(!needs_root_rewrite(&delta));
    }

    #[test]
    fn content_change_is_changed_not_added() {
        let prior = entry(4, 1);
        let next = IrEntryKey {
            intro_id: prior.intro_id,
            content_hash: [2; 32],
        };
        let stable = entry(5, 7);
        let delta = project(&[stable, prior], &[next, stable]);
        assert!(delta.added.is_empty());
        assert_eq!(delta.changed, vec![next]);
        assert!(delta.removed.is_empty());
        assert!(!delta.changed.contains(&prior));
        assert!(needs_root_rewrite(&delta));
    }

    #[test]
    fn removal_is_removed() {
        let kept = entry(1, 1);
        let gone = entry(2, 3);
        let delta = project(&[kept, gone], &[kept]);
        assert!(delta.added.is_empty());
        assert!(delta.changed.is_empty());
        assert_eq!(delta.removed, vec![gone]);
        assert!(needs_root_rewrite(&delta));
    }

    #[test]
    fn root_rewrite_is_false_only_for_empty_delta() {
        assert!(!needs_root_rewrite(&Delta::empty()));
        let fresh = entry(6, 4);
        let added = project(&[], &[fresh]);
        assert_eq!(added.added, vec![fresh]);
        assert!(added.changed.is_empty());
        assert!(needs_root_rewrite(&added));
    }

    /// The merge and the map oracle must classify the same multiset, including
    /// duplicate intro ids (last input wins) and a shuffled order.
    #[test]
    fn merge_matches_map_oracle() {
        let mut prior = Vec::new();
        let mut next = Vec::new();
        for n in 0..200u8 {
            prior.push(entry(n, n.wrapping_mul(3)));
            let hash = if n % 17 == 0 {
                n.wrapping_add(1)
            } else {
                n.wrapping_mul(3)
            };
            if n % 11 != 0 {
                next.push(entry(n, hash));
            }
        }
        next.push(entry(201, 1));
        // Duplicate intro id: last write wins on both paths.
        prior.push(entry(4, 9));
        next.push(entry(4, 8));
        prior.reverse();
        let mut left = project(&prior, &next);
        let mut right = project_via_map(&prior, &next);
        let sort = |delta: &mut Delta<IrEntryKey>| {
            delta.added.sort_by_key(|e| e.intro_id);
            delta.changed.sort_by_key(|e| e.intro_id);
            delta.removed.sort_by_key(|e| e.intro_id);
        };
        sort(&mut left);
        sort(&mut right);
        assert_eq!(left, right);
    }

    /// Agents iterate against this. The merge must beat the hex-and-tree oracle
    /// on a few thousand entries. A rewrite that makes `project` call
    /// `project_via_map` fails here. Wall time under load can swing, so the
    /// bound is 2× over a long loop, not a single shot.
    #[test]
    fn merge_beats_map_on_a_few_thousand_entries() {
        const N: u32 = 4096;
        let prior: Vec<IrEntryKey> = (0..N)
            .map(|n| IrEntryKey {
                intro_id: n.to_le_bytes().repeat(8).try_into().unwrap(),
                content_hash: [1; 32],
            })
            .collect();
        let next: Vec<IrEntryKey> = (0..N)
            .map(|n| IrEntryKey {
                intro_id: n.to_le_bytes().repeat(8).try_into().unwrap(),
                content_hash: if n % 64 == 0 { [2; 32] } else { [1; 32] },
            })
            .collect();
        let loops = 20u32;
        let merge_ns = time_ns(|| {
            for _ in 0..loops {
                std::hint::black_box(project(&prior, &next));
            }
        });
        let map_ns = time_ns(|| {
            for _ in 0..loops {
                std::hint::black_box(project_via_map(&prior, &next));
            }
        });
        eprintln!(
            "cost case=frontier/ir_project entries={N} loops={loops} merge_ns={merge_ns} map_ns={map_ns}"
        );
        assert!(
            merge_ns.saturating_mul(2) < map_ns,
            "merge {merge_ns} ns was not 2× under map {map_ns} ns — the hot path regressed toward the tree"
        );
    }

    /// Radix preparation must classify like the comparison sort, and it must
    /// beat that sort on a large random id set. Putting `latest_by_cmp` back
    /// inside `project` makes `radix_ns` and `cmp_ns` the same algorithm, so
    /// the 2× gate fails.
    #[test]
    fn radix_project_matches_and_beats_comparison_sort() {
        let mut prior = Vec::with_capacity(32_768);
        let mut next = Vec::with_capacity(32_768);
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut ident = || {
            let mut id = [0u8; 32];
            for chunk in id.chunks_mut(8) {
                state = state
                    .wrapping_mul(0x9e37_79b9_7f4a_7c15)
                    .wrapping_add(0x6a09_e667);
                chunk.copy_from_slice(&state.to_le_bytes());
            }
            id
        };
        for n in 0..32_768u32 {
            let id = ident();
            prior.push(IrEntryKey {
                intro_id: id,
                content_hash: [1; 32],
            });
            let mut hash = [1u8; 32];
            if n % 32 == 0 {
                hash[0] = 2;
            }
            if n % 17 != 0 {
                next.push(IrEntryKey {
                    intro_id: id,
                    content_hash: hash,
                });
            }
            if n == 10 {
                prior.push(IrEntryKey {
                    intro_id: id,
                    content_hash: [9; 32],
                });
            }
        }
        let mut left = project(&prior, &next);
        let mut right = project_via_cmp(&prior, &next);
        let sort = |delta: &mut Delta<IrEntryKey>| {
            delta.added.sort_by_key(|entry| entry.intro_id);
            delta.changed.sort_by_key(|entry| entry.intro_id);
            delta.removed.sort_by_key(|entry| entry.intro_id);
        };
        sort(&mut left);
        sort(&mut right);
        assert_eq!(left, right);

        let loops = 8u32;
        let radix_ns = time_ns(|| {
            for _ in 0..loops {
                std::hint::black_box(project(&prior, &next));
            }
        });
        let cmp_ns = time_ns(|| {
            for _ in 0..loops {
                std::hint::black_box(project_via_cmp(&prior, &next));
            }
        });
        eprintln!(
            "cost case=frontier/ir_radix entries=32768 loops={loops} radix_ns={radix_ns} cmp_ns={cmp_ns}"
        );
        assert!(
            radix_ns.saturating_mul(2) < cmp_ns,
            "radix {radix_ns} ns was not 2× under comparison sort {cmp_ns} ns"
        );
    }

    fn time_ns(body: impl FnOnce()) -> u128 {
        let start = std::time::Instant::now();
        body();
        start.elapsed().as_nanos()
    }
}
