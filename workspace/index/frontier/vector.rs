//! Choose which Qdrant points an upsert must rewrite.
//!
//! A point present on both sides with the same content hash is skipped. The
//! choice is a sort and a two-pointer merge on `(package, intro)`, not a
//! formatted string in a tree. [`upserts_required_via_keys`] is the tree
//! oracle the bench has to beat.
//!
//! [`UpsertLedger`] is the online form of that merge: one hash per point
//! already written. A caller records a point only after the upsert returns.

use std::collections::HashMap;

use smol_str::SmolStr;

use crate::delta::{ContentKey, content_delta};

const DOMAIN: &str = "vector-point";

/// One vector point: package, intro id, and the hash of the payload to upsert.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PointId {
    /// Package the point belongs to.
    pub package: SmolStr,
    /// Lowercase hex intro id within `package`.
    pub intro_hex: SmolStr,
    /// Content hash of the point payload. Equal hashes are not rewritten.
    pub content_hash: [u8; 32],
}

impl PointId {
    fn identity(&self) -> SmolStr {
        let package_len = self.package.len();
        SmolStr::new(format!(
            "{package_len:016x}{}{}",
            self.package, self.intro_hex
        ))
    }

    fn content_key(&self) -> ContentKey {
        ContentKey::hash(DOMAIN, self.identity(), &self.content_hash)
    }
}

/// Points in `next` that are already in `prior` with an equal content hash.
pub fn upserts_to_skip<'a>(prior: &'a [PointId], next: &'a [PointId]) -> Vec<&'a PointId> {
    let required = required_ids(prior, next);
    next.iter()
        .filter(|point| !required.contains(&(point.package.as_str(), point.intro_hex.as_str())))
        .collect()
}

/// Points in `next` that are new, or whose content hash differs from `prior`.
pub fn upserts_required<'a>(prior: &'a [PointId], next: &'a [PointId]) -> Vec<&'a PointId> {
    let required = required_ids(prior, next);
    next.iter()
        .filter(|point| required.contains(&(point.package.as_str(), point.intro_hex.as_str())))
        .collect()
}

/// Tree-and-string twin of [`upserts_required`]. Not the function callers use.
pub fn upserts_required_via_keys<'a>(
    prior: &'a [PointId],
    next: &'a [PointId],
) -> Vec<&'a PointId> {
    let required = required_ids_via_keys(prior, next);
    next.iter()
        .filter(|point| required.contains(&point.identity()))
        .collect()
}

fn required_ids<'a>(
    prior: &'a [PointId],
    next: &'a [PointId],
) -> std::collections::HashSet<(&'a str, &'a str)> {
    let prior_idx = latest_idx(prior);
    let next_idx = latest_idx(next);
    let mut required = std::collections::HashSet::new();
    let mut i = 0;
    let mut j = 0;
    while i < prior_idx.len() && j < next_idx.len() {
        let prior_point = &prior[prior_idx[i]];
        let next_point = &next[next_idx[j]];
        match key(prior_point).cmp(&key(next_point)) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => {
                required.insert(key(next_point));
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                if prior_point.content_hash != next_point.content_hash {
                    required.insert(key(next_point));
                }
                i += 1;
                j += 1;
            }
        }
    }
    while j < next_idx.len() {
        required.insert(key(&next[next_idx[j]]));
        j += 1;
    }
    required
}

fn key(point: &PointId) -> (&str, &str) {
    (point.package.as_str(), point.intro_hex.as_str())
}

/// Indices of the last input occurrence of each `(package, intro)`, ordered by
/// that pair. Sorting with the input index descending puts the last write
/// first.
fn latest_idx(points: &[PointId]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by(|&i, &j| key(&points[i]).cmp(&key(&points[j])).then(j.cmp(&i)));
    let mut out = Vec::with_capacity(points.len());
    let mut previous: Option<(&str, &str)> = None;
    for index in order {
        let id = key(&points[index]);
        if previous == Some(id) {
            continue;
        }
        previous = Some(id);
        out.push(index);
    }
    out
}

fn required_ids_via_keys(
    prior: &[PointId],
    next: &[PointId],
) -> std::collections::BTreeSet<SmolStr> {
    let prior_keys: Vec<ContentKey> = prior.iter().map(PointId::content_key).collect();
    let next_keys: Vec<ContentKey> = next.iter().map(PointId::content_key).collect();
    let delta = content_delta(&prior_keys, &next_keys);
    delta
        .added
        .into_iter()
        .chain(delta.changed)
        .map(|key| key.id)
        .collect()
}

/// Hashes of points whose upsert has already succeeded.
#[derive(Debug, Default)]
pub struct UpsertLedger {
    written: HashMap<(SmolStr, SmolStr), [u8; 32]>,
}

impl UpsertLedger {
    /// Empty ledger. Every point needs a write.
    #[must_use]
    pub fn new() -> Self {
        Self {
            written: HashMap::new(),
        }
    }

    /// Whether `point` is absent or its content hash differs from the last
    /// successful upsert.
    #[must_use]
    pub fn needs_write(&self, point: &PointId) -> bool {
        self.written
            .get(&(point.package.clone(), point.intro_hex.clone()))
            .is_none_or(|hash| *hash != point.content_hash)
    }

    /// Record points whose upsert returned successfully.
    pub fn commit(&mut self, points: &[PointId]) {
        for point in points {
            self.restore(
                point.package.as_str(),
                point.intro_hex.as_str(),
                point.content_hash,
            );
        }
    }

    /// Install one hash loaded from scratch. Same effect as [`Self::commit`]
    /// for a single point that is already known to have been written.
    pub fn restore(&mut self, package: &str, intro_hex: &str, content_hash: [u8; 32]) {
        self.written.insert(
            (SmolStr::new(package), SmolStr::new(intro_hex)),
            content_hash,
        );
    }

    /// Drop every hash for `package` after its Qdrant points are deleted.
    pub fn forget_package(&mut self, package: &str) {
        self.written.retain(|(stored, _), _| stored != package);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(package: &str, intro_hex: &str, hash_byte: u8) -> PointId {
        PointId {
            package: SmolStr::new(package),
            intro_hex: SmolStr::new(intro_hex),
            content_hash: [hash_byte; 32],
        }
    }

    #[test]
    fn skip_plus_required_equals_next_len_when_prior_is_subset() {
        let kept_a = point("serde", "aa", 1);
        let kept_b = point("serde", "bb", 2);
        let fresh = point("tokio", "cc", 3);
        let prior = vec![kept_b.clone(), kept_a.clone()];
        let next = vec![kept_a.clone(), fresh.clone(), kept_b.clone()];
        assert!(prior.iter().all(|existing| next.contains(existing)));

        let skip = upserts_to_skip(&prior, &next);
        let required = upserts_required(&prior, &next);

        assert_eq!(skip.len() + required.len(), next.len());
        assert_eq!(skip.len(), prior.len());
        assert_eq!(required.len(), next.len() - prior.len());
        assert!(skip.iter().all(|point| !required.contains(point)));
        assert!(skip.iter().any(|point| std::ptr::eq(*point, &next[0])));
        assert!(skip.iter().any(|point| std::ptr::eq(*point, &next[2])));
        assert!(required.iter().any(|point| std::ptr::eq(*point, &next[1])));
    }

    #[test]
    fn changed_content_hash_is_required_not_skipped() {
        let kept = point("serde", "aa", 1);
        let before = point("serde", "bb", 2);
        let rewritten = point("serde", "bb", 9);
        let prior = vec![kept.clone(), before];
        let next = vec![rewritten, kept.clone()];

        let skip = upserts_to_skip(&prior, &next);
        let required = upserts_required(&prior, &next);

        assert_eq!(skip.len() + required.len(), next.len());
        assert_eq!(skip.len(), 1);
        assert_eq!(required.len(), 1);
        assert!(std::ptr::eq(skip[0], &next[1]));
        assert!(std::ptr::eq(required[0], &next[0]));
    }

    #[test]
    fn package_prefix_does_not_collapse_into_intro() {
        let prior = vec![point("ab", "c", 1)];
        let next = vec![point("a", "bc", 1)];
        assert!(upserts_to_skip(&prior, &next).is_empty());
        assert_eq!(upserts_required(&prior, &next).len(), 1);
    }

    #[test]
    fn merge_matches_the_string_tree() {
        let mut prior = Vec::new();
        let mut next = Vec::new();
        for n in 0..200u32 {
            let package = format!("pkg{n:04}");
            let intro = format!("{n:08x}");
            prior.push(PointId {
                package: SmolStr::new(&package),
                intro_hex: SmolStr::new(&intro),
                content_hash: [1; 32],
            });
            if n % 5 != 0 {
                let mut hash = [1u8; 32];
                if n % 7 == 0 {
                    hash[0] = 2;
                }
                next.push(PointId {
                    package: SmolStr::new(package),
                    intro_hex: SmolStr::new(intro),
                    content_hash: hash,
                });
            }
        }
        let merge: Vec<_> = upserts_required(&prior, &next)
            .into_iter()
            .map(|point| point.identity())
            .collect();
        let tree: Vec<_> = upserts_required_via_keys(&prior, &next)
            .into_iter()
            .map(|point| point.identity())
            .collect();
        assert_eq!(merge, tree);
    }

    #[test]
    fn ledger_skips_until_the_hash_changes_and_ignores_a_failed_write() {
        let mut ledger = UpsertLedger::new();
        let written = point("serde", "aa", 1);
        assert!(ledger.needs_write(&written));
        assert!(ledger.needs_write(&written));
        ledger.commit(std::slice::from_ref(&written));
        assert!(!ledger.needs_write(&written));
        let rewritten = point("serde", "aa", 2);
        assert!(ledger.needs_write(&rewritten));
    }

    #[test]
    fn merge_beats_the_string_tree_on_a_few_thousand_points() {
        const N: u32 = 4_096;
        let prior: Vec<PointId> = (0..N)
            .map(|n| PointId {
                package: SmolStr::new(format!("package-{n:05}")),
                intro_hex: SmolStr::new(format!("{n:08x}")),
                content_hash: [1; 32],
            })
            .collect();
        let next: Vec<PointId> = (0..N)
            .map(|n| PointId {
                package: SmolStr::new(format!("package-{n:05}")),
                intro_hex: SmolStr::new(format!("{n:08x}")),
                content_hash: if n % 64 == 0 { [2; 32] } else { [1; 32] },
            })
            .collect();
        let loops = 8u32;
        let merge_ns = time(|| {
            for _ in 0..loops {
                std::hint::black_box(upserts_required(&prior, &next));
            }
        });
        let tree_ns = time(|| {
            for _ in 0..loops {
                std::hint::black_box(upserts_required_via_keys(&prior, &next));
            }
        });
        eprintln!(
            "cost case=frontier/vector_project points={N} loops={loops} merge_ns={merge_ns} tree_ns={tree_ns}"
        );
        assert!(
            merge_ns.saturating_mul(2) < tree_ns,
            "merge {merge_ns} ns was not 2× under the string tree {tree_ns} ns"
        );
    }

    fn time(body: impl FnOnce()) -> u128 {
        let start = std::time::Instant::now();
        body();
        start.elapsed().as_nanos()
    }
}
