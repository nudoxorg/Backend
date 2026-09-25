//! Choose which Qdrant points an upsert must rewrite.
//!
//! A point present on both sides with the same content hash is skipped. This
//! module does not talk to a Qdrant client; it only names the work set.

use std::collections::BTreeSet;

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
    /// Fixed-width length prefix so `"ab"+"c"` and `"a"+"bc"` stay distinct
    /// ids.
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
///
/// References point at `next`. These upserts are the work that can be skipped.
pub fn upserts_to_skip<'a>(prior: &'a [PointId], next: &'a [PointId]) -> Vec<&'a PointId> {
    let required = required_ids(prior, next);
    next.iter()
        .filter(|point| !required.contains(&point.identity()))
        .collect()
}

/// Points in `next` that are new, or whose content hash differs from `prior`.
///
/// References point at `next`. With [`upserts_to_skip`], this partitions
/// `next`.
pub fn upserts_required<'a>(prior: &'a [PointId], next: &'a [PointId]) -> Vec<&'a PointId> {
    let required = required_ids(prior, next);
    next.iter()
        .filter(|point| required.contains(&point.identity()))
        .collect()
}

fn required_ids(prior: &[PointId], next: &[PointId]) -> BTreeSet<SmolStr> {
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
        assert!(
            prior.iter().all(|existing| next.contains(existing)),
            "prior must be a subset of next"
        );

        let skip = upserts_to_skip(&prior, &next);
        let required = upserts_required(&prior, &next);

        assert_eq!(
            skip.len() + required.len(),
            next.len(),
            "differential: skipped upserts plus required upserts cover next"
        );
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
}
