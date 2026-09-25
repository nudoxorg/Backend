//! RustSec advisory tree.
//!
//! cargo-audit and cargo-deny read the same TOML files. One poll hashes every
//! `*.toml` under the root, compares that map with the watermark, and emits
//! catalog ops only for files whose bytes changed. An unchanged tree emits no
//! ops. A file that does not parse stays in the cursor so one poison document
//! does not stall the rest; editing it changes the hash and retries.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::ingest::{
    advisory::parse_rustsec,
    follower::{Follower, FollowerBatch, FollowerError, PollCadence},
    watermark::FeedWatermark,
};

const FEED_ID: &str = "rustsec-crates";

/// Poll a directory of RustSec advisory TOML files.
pub struct RustsecFollower {
    root: PathBuf,
}

impl RustsecFollower {
    /// `root` is the advisory-db checkout (or any directory of `*.toml` files).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Follower for RustsecFollower {
    fn feed_id(&self) -> &str {
        FEED_ID
    }

    fn cadence(&self) -> PollCadence {
        PollCadence::EverySeconds(60 * 60)
    }

    fn poll(
        &self,
        previous: Option<&FeedWatermark>,
        now_unix_ms: i64,
    ) -> Result<FollowerBatch, FollowerError> {
        let current = snapshot(&self.root)?;
        let prior = previous
            .and_then(|watermark| watermark.last_ref.as_deref())
            .map(decode)
            .unwrap_or_default();
        let mut ops = Vec::new();
        for (path, hash) in &current {
            if prior.get(path).is_some_and(|seen| seen == hash) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(self.root.join(path)) else {
                continue;
            };
            let Ok(source) = parse_rustsec(&text, now_unix_ms) else {
                continue;
            };
            ops.extend(source.catalog_ops());
        }
        Ok(FollowerBatch {
            ops,
            next_watermark: FeedWatermark {
                feed: FEED_ID.to_owned(),
                last_ref: Some(encode(&current)),
                last_checked_at: now_unix_ms,
                last_error: None,
            },
            caught_up: true,
        })
    }
}

fn snapshot(root: &Path) -> Result<BTreeMap<String, String>, FollowerError> {
    let mut files = BTreeMap::new();
    if root.is_dir() {
        walk(root, root, &mut files)?;
    }
    Ok(files)
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<(), FollowerError> {
    let entries = std::fs::read_dir(dir).map_err(|err| FollowerError::Parse {
        feed: FEED_ID.to_owned(),
        detail: err.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| FollowerError::Parse {
            feed: FEED_ID.to_owned(),
            detail: err.to_string(),
        })?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|err| FollowerError::Parse {
            feed: FEED_ID.to_owned(),
            detail: err.to_string(),
        })?;
        if kind.is_dir() {
            walk(root, &path, out)?;
            continue;
        }
        if !kind.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|err| FollowerError::Parse {
            feed: FEED_ID.to_owned(),
            detail: err.to_string(),
        })?;
        let key = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        out.insert(key, blake3::hash(&bytes).to_hex().to_string());
    }
    Ok(())
}

fn encode(files: &BTreeMap<String, String>) -> String {
    files
        .iter()
        .map(|(path, hash)| format!("{hash}\t{path}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn decode(raw: &str) -> BTreeMap<String, String> {
    raw.lines()
        .filter_map(|line| {
            let (hash, path) = line.split_once('\t')?;
            Some((path.to_owned(), hash.to_owned()))
        })
        .collect()
}
