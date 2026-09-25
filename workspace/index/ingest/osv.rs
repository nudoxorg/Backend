//! OSV modified-id follower.
//!
//! One poll reads `{ecosystem}/modified_id.csv`, keeps rows strictly newer
//! than the cursor, and fetches each `{id}.json`. [`crate::ingest::advisory::resolve_osv`]
//! turns a document into `UpsertAdvisory` ops. A row at or before the cursor
//! is not fetched. A document that does not parse is skipped so one poison
//! file does not stall the cursor. The cursor is the modified timestamp of
//! the last row applied on this page.

use crate::ingest::advisory::resolve_osv;
use crate::ingest::follower::{Follower, FollowerBatch, FollowerError, PollCadence};
use crate::ingest::transport::{FeedRequest, FeedResponse, FeedTransport};
use crate::ingest::watermark::FeedWatermark;
const PAGE: usize = 8;
const FEED_ID: &str = "osv-crates.io";
const INDEX_URL: &str =
    "https://osv-vulnerabilities.storage.googleapis.com/crates.io/modified_id.csv";
const DOC_PREFIX: &str = "https://osv-vulnerabilities.storage.googleapis.com/crates.io/";

/// One row of `modified_id.csv`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModifiedRow {
    id: String,
    modified: String,
}

/// Poll the crates.io OSV modified index and the vulnerability documents it names.
pub struct OsvFollower<Transport> {
    transport: Transport,
}

impl<Transport> OsvFollower<Transport> {
    /// A follower against the public crates.io OSV bucket.
    pub fn new(transport: Transport) -> Self {
        Self { transport }
    }
}

impl<Transport: FeedTransport> Follower for OsvFollower<Transport> {
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
        let (since, etag) = split_cursor(previous.and_then(|watermark| watermark.last_ref.as_deref()));
        let request = FeedRequest::conditional(INDEX_URL, etag);
        let response = self.transport.fetch(&request)?;
        match response {
            FeedResponse::NotModified => Ok(FollowerBatch {
                ops: Vec::new(),
                next_watermark: FeedWatermark {
                    feed: FEED_ID.to_owned(),
                    last_ref: previous.and_then(|watermark| watermark.last_ref.clone()),
                    last_checked_at: now_unix_ms,
                    last_error: None,
                },
                caught_up: true,
            }),
            FeedResponse::Modified { body, etag } => {
                let rows = rows_after(&body, &since);
                let page = rows.iter().take(PAGE).cloned().collect::<Vec<_>>();
                let mut ops = Vec::new();
                for row in &page {
                    let url = format!("{DOC_PREFIX}{}.json", row.id);
                    let document = self.transport.fetch(&FeedRequest::get(url))?;
                    let FeedResponse::Modified { body, .. } = document else {
                        continue;
                    };
                    match resolve_osv(&body, now_unix_ms) {
                        Ok(sources) => ops.extend(sources.into_iter().map(|source| source.to_upsert_op())),
                        Err(_) => continue,
                    }
                }
                let cursor = page.last().map(|row| row.modified.as_str()).unwrap_or(&since);
                Ok(FollowerBatch {
                    ops,
                    next_watermark: FeedWatermark {
                        feed: FEED_ID.to_owned(),
                        last_ref: Some(join_cursor(cursor, etag.as_deref())),
                        last_checked_at: now_unix_ms,
                        last_error: None,
                    },
                    caught_up: page.len() == rows.len(),
                })
            }
        }
    }
}

fn split_cursor(raw: Option<&str>) -> (String, Option<String>) {
    let Some(raw) = raw else {
        return (String::new(), None);
    };
    match raw.split_once('\u{1}') {
        Some((modified, etag)) if !etag.is_empty() => (modified.to_owned(), Some(etag.to_owned())),
        Some((modified, _)) => (modified.to_owned(), None),
        None => (raw.to_owned(), None),
    }
}

fn join_cursor(modified: &str, etag: Option<&str>) -> String {
    match etag {
        Some(etag) => format!("{modified}\u{1}{etag}"),
        None => modified.to_owned(),
    }
}

/// Rows strictly newer than `since`, oldest first. A repeated id keeps the
/// newest modified timestamp. The header line and rows without a comma are
/// dropped. `AB` and `A` stay distinct ids.
fn rows_after(body: &[u8], since: &str) -> Vec<ModifiedRow> {
    let text = String::from_utf8_lossy(body);
    let mut by_id: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for line in text.lines() {
        let Some((id, modified)) = line.split_once(',') else {
            continue;
        };
        let id = id.trim();
        let modified = modified.trim();
        if id.is_empty() || id == "id" || modified.is_empty() || modified <= since {
            continue;
        }
        let replace = by_id
            .get(id)
            .is_none_or(|current| modified > current.as_str());
        if replace {
            by_id.insert(id.to_owned(), modified.to_owned());
        }
    }
    let mut rows: Vec<ModifiedRow> = by_id
        .into_iter()
        .map(|(id, modified)| ModifiedRow { id, modified })
        .collect();
    rows.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.id.cmp(&right.id))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::follower::Follower;
    use crate::ingest::transport::FixtureTransport;

    fn index_body() -> &'static str {
        "id,modified\n\
         RUSTSEC-OLD,2020-01-01T00:00:00Z\n\
         A,2021-01-01T00:00:00Z\n\
         AB,2021-01-01T00:00:00Z\n\
         not-a-row\n\
         RUSTSEC-1,2022-06-01T00:00:00Z\n\
         RUSTSEC-1,2021-01-01T00:00:00Z\n"
    }

    fn vuln(id: &str, name: &str) -> String {
        format!(
            r#"{{"id":"{id}","affected":[{{"package":{{"ecosystem":"crates.io","name":"{name}"}}}}]}}"#
        )
    }

    #[test]
    fn poll_skips_the_cursor_and_a_replay_fetches_nothing() {
        let index = "https://osv-vulnerabilities.storage.googleapis.com/crates.io/modified_id.csv";
        let transport = FixtureTransport::new()
            .with(index, index_body().as_bytes(), Some("v1"))
            .with(
                "https://osv-vulnerabilities.storage.googleapis.com/crates.io/A.json",
                vuln("A", "serde").into_bytes(),
                None,
            )
            .with(
                "https://osv-vulnerabilities.storage.googleapis.com/crates.io/AB.json",
                vuln("AB", "serde_json").into_bytes(),
                None,
            )
            .with(
                "https://osv-vulnerabilities.storage.googleapis.com/crates.io/RUSTSEC-1.json",
                vuln("RUSTSEC-1", "smallvec").into_bytes(),
                None,
            );
        let follower = OsvFollower::new(transport);
        let previous = FeedWatermark {
            feed: FEED_ID.to_owned(),
            last_ref: Some("2020-01-01T00:00:00Z".to_owned()),
            last_checked_at: 1,
            last_error: None,
        };
        let first = follower.poll(Some(&previous), 50).expect("poll");
        assert_eq!(first.ops.len(), 3, "A, AB, and the newer RUSTSEC-1 each emit one op");
        assert!(first.caught_up);
        let again = follower.poll(Some(&first.next_watermark), 60).expect("replay");
        assert!(again.ops.is_empty());
        assert!(again.caught_up);
    }

    #[test]
    fn a_poison_document_is_skipped_and_the_cursor_still_advances() {
        let index = "https://osv-vulnerabilities.storage.googleapis.com/crates.io/modified_id.csv";
        let body = "id,modified\nBAD,2023-01-01T00:00:00Z\nGOOD,2023-02-01T00:00:00Z\n";
        let transport = FixtureTransport::new()
            .with(index, body.as_bytes(), Some("v2"))
            .with(
                "https://osv-vulnerabilities.storage.googleapis.com/crates.io/BAD.json",
                b"{}".to_vec(),
                None,
            )
            .with(
                "https://osv-vulnerabilities.storage.googleapis.com/crates.io/GOOD.json",
                vuln("GOOD", "libc").into_bytes(),
                None,
            );
        let follower = OsvFollower::new(transport);
        let batch = follower.poll(None, 70).expect("poll");
        assert_eq!(batch.ops.len(), 1);
        let cursor = batch.next_watermark.last_ref.expect("cursor");
        assert!(cursor.starts_with("2023-02-01T00:00:00Z"));
    }
}
