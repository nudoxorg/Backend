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
    let prior = latest_by_id(prior);
    let next = latest_by_id(next);
    let mut delta = Delta::empty();
    let mut i = 0;
    let mut j = 0;
    while i < prior.len() && j < next.len() {
        match prior[i].intro_id.cmp(&next[j].intro_id) {
            std::cmp::Ordering::Less => {
                delta.removed.push(prior[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                delta.added.push(next[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                if prior[i].content_hash != next[j].content_hash {
                    delta.changed.push(next[j]);
                }
                i += 1;
                j += 1;
            }
        }
    }
    delta.removed.extend_from_slice(&prior[i..]);
    delta.added.extend_from_slice(&next[j..]);
    delta
}

/// Reference classifier: hex id into a `BTreeMap`, then [`content_delta`].
///
/// Kept so benches and the differential test can show the merge was not
/// replaced by this path. Not the function callers should use.
pub fn project_via_map(prior: &[IrEntryKey], next: &[IrEntryKey]) -> Delta<IrEntryKey> {
    use std::collections::BTreeMap;

    use smol_str::SmolStr;

    use crate::delta::{ContentKey, content_delta};

    fn hex_id(intro_id: &[u8; 32]) -> SmolStr {
        SmolStr::new(data_encoding::HEXLOWER.encode(intro_id))
    }
    fn key(entry: IrEntryKey) -> ContentKey {
        ContentKey::hash("ir-entry", hex_id(&entry.intro_id), &entry.content_hash)
    }

    let prior_keys: Vec<ContentKey> = prior.iter().copied().map(key).collect();
    let next_keys: Vec<ContentKey> = next.iter().copied().map(key).collect();
    let classified = content_delta(&prior_keys, &next_keys);
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
fn latest_by_id(entries: &[IrEntryKey]) -> Vec<IrEntryKey> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&i, &j| {
        entries[i]
            .intro_id
            .cmp(&entries[j].intro_id)
            .then(j.cmp(&i))
    });
    let mut out = Vec::with_capacity(entries.len());
    let mut previous: Option<[u8; 32]> = None;
    for index in order {
        let id = entries[index].intro_id;
        if previous == Some(id) {
            continue;
        }
        previous = Some(id);
        out.push(entries[index]);
    }
    out
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
            let hash = if n % 17 == 0 { n.wrapping_add(1) } else { n.wrapping_mul(3) };
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

    fn time_ns(body: impl FnOnce()) -> u128 {
        let start = std::time::Instant::now();
        body();
        start.elapsed().as_nanos()
    }
}
