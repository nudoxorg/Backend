//! Project IR entries through [`content_delta`](crate::delta::content_delta).
//!
//! An intro id whose content hash is unchanged is omitted from every set. Same
//! hash means skip: reordering an entry is not a root rewrite.

use std::collections::BTreeMap;

use smol_str::SmolStr;

use crate::delta::{ContentKey, Delta, content_delta};

const DOMAIN: &str = "ir-entry";

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

impl IrEntryKey {
    fn content_key(self) -> ContentKey {
        ContentKey::hash(DOMAIN, intro_id_hex(&self.intro_id), &self.content_hash)
    }
}

/// Diff `prior` and `next` by intro id, using the content hash as the payload.
///
/// Unchanged hashes are absent from every set. A known intro id with a new hash
/// is [`Delta::changed`](Delta::changed), not added. An intro id missing from
/// `next` is [`Delta::removed`](Delta::removed).
pub fn project(prior: &[IrEntryKey], next: &[IrEntryKey]) -> Delta<IrEntryKey> {
    let prior_keys: Vec<ContentKey> = prior.iter().copied().map(IrEntryKey::content_key).collect();
    let next_keys: Vec<ContentKey> = next.iter().copied().map(IrEntryKey::content_key).collect();
    let classified = content_delta(&prior_keys, &next_keys);

    // Next overwrites prior so a changed intro id resolves to the new hash.
    // Removed ids are only in `prior`, so they keep the hash that disappeared.
    let mut by_id = BTreeMap::<SmolStr, IrEntryKey>::new();
    for entry in prior {
        by_id.insert(intro_id_hex(&entry.intro_id), *entry);
    }
    for entry in next {
        by_id.insert(intro_id_hex(&entry.intro_id), *entry);
    }

    Delta {
        added: entries_for(&classified.added, &by_id),
        changed: entries_for(&classified.changed, &by_id),
        removed: entries_for(&classified.removed, &by_id),
    }
}

/// Whether applying `delta` must rewrite the IR root.
///
/// False only when `delta` is empty.
pub fn needs_root_rewrite(delta: &Delta<IrEntryKey>) -> bool {
    !delta.is_empty()
}

fn intro_id_hex(intro_id: &[u8; 32]) -> SmolStr {
    SmolStr::new(data_encoding::HEXLOWER.encode(intro_id))
}

fn entries_for(keys: &[ContentKey], by_id: &BTreeMap<SmolStr, IrEntryKey>) -> Vec<IrEntryKey> {
    let entries: Vec<IrEntryKey> = keys
        .iter()
        .filter_map(|key| by_id.get(&key.id).copied())
        .collect();
    debug_assert_eq!(entries.len(), keys.len());
    entries
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
}
