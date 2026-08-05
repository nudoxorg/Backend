//! Parsing raw `git ls-remote --tags --heads <url>` output into versions
//! (REGISTRYLESS §6.3, RL-14).
//!
//! The IO layer runs `ls-remote` via grit and hands the raw bytes here; this
//! module stays pure. Each output line is `<oid>\t<ref>`. Annotated tags emit a
//! second peeled line `<oid>\t<ref>^{}` whose oid is the *commit* the tag points
//! at — we prefer that peeled oid so `source_rev` pins the real commit, not the
//! tag object. The peeled oid is recorded in the `ListedVersion.raw` slot as
//! `"<tag>@<oid>"` so the ingestor can pin the revision without a second call.

use std::collections::BTreeMap;

use smol_str::SmolStr;

use crate::ecosystem::{
    cpp::version::CppVersion,
    upstream::{ListedVersion, ListingStatus},
};

/// The `refs/tags/` prefix that marks a tag ref in `ls-remote` output.
const TAG_PREFIX: &str = "refs/tags/";

/// The `refs/heads/` prefix that marks a branch ref in `ls-remote` output.
const HEAD_PREFIX: &str = "refs/heads/";

/// The suffix git appends to the dereferenced (peeled) entry of an annotated
/// tag.
const PEELED_SUFFIX: &str = "^{}";

/// One resolved tag: its short name and the best-known object id (peeled when
/// available).
struct TagEntry {
    object_id: SmolStr,
    /// `true` once a peeled (`^{}`) line has upgraded the object id to the
    /// underlying commit; a later non-peeled duplicate must not overwrite it.
    peeled: bool,
}

/// Parse `ls-remote` bytes into listed versions (REGISTRYLESS §6.3).
///
/// - Tags become versions; the peeled commit oid is preferred over the tag oid.
/// - `refs/heads/` (branch) refs are ignored here (branches become
///   pseudo-versions at HEAD in a later wave, not tag versions).
/// - Malformed lines, CRLF, empty input, and a tag literally named `^{}` are
///   tolerated; the parser never panics and never yields a duplicate tag.
pub fn parse_ls_remote(body: &[u8]) -> Vec<ListedVersion<CppVersion>> {
    let Ok(text) = std::str::from_utf8(body) else {
        return Vec::new();
    };

    // Preserve first-seen order for determinism while de-duplicating and letting
    // peeled lines upgrade the recorded oid.
    let mut order: Vec<SmolStr> = Vec::new();
    let mut tags: BTreeMap<SmolStr, TagEntry> = BTreeMap::new();

    for raw_line in text.split('\n') {
        let line = raw_line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let Some((object_id, reference)) = line.split_once('\t') else {
            continue; // malformed: no tab separator
        };
        let object_id = object_id.trim();
        let reference = reference.trim();
        if object_id.is_empty() {
            continue;
        }

        if let Some(after) = reference.strip_prefix(TAG_PREFIX) {
            record_tag(after, object_id, &mut order, &mut tags);
        }
        // Branches (`refs/heads/…`) and other refs (`HEAD`, `refs/pull/…`) are
        // deliberately skipped on the tag-listing path.
        let _ = HEAD_PREFIX;
    }

    order
        .into_iter()
        .filter_map(|tag_name| {
            let entry = tags.get(&tag_name)?;
            let version = CppVersion::parse_lossless(&tag_name);
            // Record `<tag>@<oid>` so the ingestor pins source_rev (RL-14).
            let raw = SmolStr::from(format!("{tag_name}@{}", entry.object_id));
            Some(ListedVersion {
                version,
                status: ListingStatus::Listed,
                raw,
            })
        })
        .collect()
}

/// Record (or upgrade) one tag ref. A peeled (`^{}`) entry replaces the tag
/// object's oid with the underlying commit oid.
fn record_tag(
    after_prefix: &str,
    object_id: &str,
    order: &mut Vec<SmolStr>,
    tags: &mut BTreeMap<SmolStr, TagEntry>,
) {
    // A tag literally named `^{}` yields `after_prefix == "^{}"` with no name
    // before the suffix — treat it as a real tag named `^{}`, not a peel marker.
    let (tag_name, is_peeled) = match after_prefix.strip_suffix(PEELED_SUFFIX) {
        Some(name) if !name.is_empty() => (name, true),
        _ => (after_prefix, false),
    };
    let key = SmolStr::from(tag_name);
    match tags.get_mut(&key) {
        Some(existing) => {
            // Only a peeled line may overwrite; a peel always wins over a
            // non-peel, and a later non-peel never clobbers a recorded peel.
            if is_peeled && !existing.peeled {
                existing.object_id = SmolStr::from(object_id);
                existing.peeled = true;
            }
        }
        None => {
            order.push(key.clone());
            tags.insert(
                key,
                TagEntry {
                    object_id: SmolStr::from(object_id),
                    peeled: is_peeled,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests;
