//! Choose which Qdrant points an upsert must rewrite.
//!
//! A point present on both sides with the same content hash is skipped. The
//! choice is a sort and a two-pointer merge on `(package, intro)`, not a
//! formatted string in a tree. [`upserts_required_via_keys`] is the tree
//! oracle the bench has to beat.
//!
//! [`UpsertLedger`] is the online form of that merge: one hash per point
//! already written. A caller records a point only after the upsert returns.

use std::hash::{Hash, Hasher};

use hashbrown::HashMap;
use smol_str::SmolStr;

use crate::delta::{ContentKey, content_delta_via_map, merge_sorted};

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

/// Fingerprint of a symbol's embedding input.
///
/// The hash is the model id and the text, so a re-delivery of the same name
/// can skip the embedder. The embedding bytes are not the identity: they are
/// the work this fingerprint exists to avoid.
#[must_use]
pub fn symbol_fingerprint(package: &str, intro_hex: &str, model: &str, text: &str) -> PointId {
    PointId {
        package: SmolStr::new(package),
        intro_hex: SmolStr::new(intro_hex),
        content_hash: content_fingerprint(model, text),
    }
}

/// BLAKE3 of the model id and the text. The embedding bytes are not an input.
#[must_use]
pub fn content_fingerprint(model: &str, text: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(model.as_bytes());
    hasher.update(&[0xff]);
    hasher.update(text.as_bytes());
    *hasher.finalize().as_bytes()
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
    let partition = merge_sorted(
        &prior_idx,
        &next_idx,
        |&i, &j| key(&prior[i]).cmp(&key(&next[j])),
        |&i, &j| prior[i].content_hash == next[j].content_hash,
    );
    partition
        .added
        .into_iter()
        .chain(partition.changed)
        .map(|index| key(&next[index]))
        .collect()
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
    let delta = content_delta_via_map(&prior_keys, &next_keys);
    delta
        .added
        .into_iter()
        .chain(delta.changed)
        .map(|key| key.id)
        .collect()
}

/// Owned ledger key. Uuid package and intro ids are the raw 16 bytes. Any
/// other name stays a string, borrowed on lookup.
#[derive(Debug, PartialEq, Eq)]
enum WrittenKey {
    Ids([u8; 16], [u8; 16]),
    Text(SmolStr, SmolStr),
}

/// Borrowed lookup. A uuid pair hashes the same bytes as [`WrittenKey::Ids`].
enum WrittenRef<'a> {
    Ids([u8; 16], [u8; 16]),
    Text(&'a str, &'a str),
}

impl Hash for WrittenKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Ids(package, intro) => WrittenRef::Ids(*package, *intro).hash(state),
            Self::Text(package, intro) => {
                WrittenRef::Text(package.as_str(), intro.as_str()).hash(state)
            }
        }
    }
}

impl Hash for WrittenRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Ids(package, intro) => {
                0u8.hash(state);
                package.hash(state);
                intro.hash(state);
            }
            Self::Text(package, intro) => {
                1u8.hash(state);
                package.hash(state);
                intro.hash(state);
            }
        }
    }
}

impl hashbrown::Equivalent<WrittenKey> for WrittenRef<'_> {
    fn equivalent(&self, key: &WrittenKey) -> bool {
        match (self, key) {
            (Self::Ids(package, intro), WrittenKey::Ids(stored_pkg, stored_intro)) => {
                package == stored_pkg && intro == stored_intro
            }
            (Self::Text(package, intro), WrittenKey::Text(stored_pkg, stored_intro)) => {
                *package == stored_pkg.as_str() && *intro == stored_intro.as_str()
            }
            _ => false,
        }
    }
}

fn written_ref<'a>(package: &'a str, intro: &'a str) -> WrittenRef<'a> {
    match (uuid::Uuid::try_parse(package), uuid::Uuid::try_parse(intro)) {
        (Ok(package), Ok(intro)) => WrittenRef::Ids(*package.as_bytes(), *intro.as_bytes()),
        _ => WrittenRef::Text(package, intro),
    }
}

fn written_key(package: &str, intro: &str) -> WrittenKey {
    match written_ref(package, intro) {
        WrittenRef::Ids(package, intro) => WrittenKey::Ids(package, intro),
        WrittenRef::Text(package, intro) => {
            WrittenKey::Text(SmolStr::new(package), SmolStr::new(intro))
        }
    }
}

/// Hashes of points whose upsert has already succeeded.
///
/// A skip check borrows the package and intro the point already owns.
#[derive(Debug, Default)]
pub struct UpsertLedger {
    written: HashMap<WrittenKey, [u8; 32]>,
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
        self.hash_of(point.package.as_str(), point.intro_hex.as_str())
            .is_none_or(|hash| hash != point.content_hash)
    }

    /// Whether the symbol ids are absent or their content hash differs.
    ///
    /// This does not format the ids. [`Self::needs_write`] on the same ids
    /// rendered as hyphenated text answers the same question.
    #[must_use]
    pub fn needs_write_ids(
        &self,
        package: &uuid::Uuid,
        intro: &uuid::Uuid,
        content_hash: &[u8; 32],
    ) -> bool {
        self.written
            .get(&WrittenRef::Ids(*package.as_bytes(), *intro.as_bytes()))
            .is_none_or(|hash| hash != content_hash)
    }

    fn hash_of(&self, package: &str, intro_hex: &str) -> Option<[u8; 32]> {
        self.written.get(&written_ref(package, intro_hex)).copied()
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
        self.written
            .insert(written_key(package, intro_hex), content_hash);
    }

    /// Drop every hash for `package` after its Qdrant points are deleted.
    pub fn forget_package(&mut self, package: &str) {
        let ids = uuid::Uuid::try_parse(package)
            .ok()
            .map(|package| *package.as_bytes());
        self.written.retain(|key, _| match key {
            WrittenKey::Ids(stored, _) => ids.as_ref() != Some(stored),
            WrittenKey::Text(stored, _) => stored != package,
        });
    }
}

#[cfg(test)]
mod tests;
