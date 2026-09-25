//! npm replicate `_changes` follower.
//!
//! The feed is CouchDB's sequence log:
//! `GET {base}/_changes?since={seq}&limit={n}&include_docs=true`.
//! A row with `dist-tags.latest` becomes [`CatalogEvent::Published`]. A
//! deleted row becomes [`CatalogEvent::Withdrawn`] only when that same doc
//! still names a latest version. A delete with no version is dropped: the
//! catalog event requires a version, and inventing one would withdraw the
//! wrong release.
//!
//! The cursor is the numeric `last_seq`. Rows at or below the caller's `since`
//! are ignored, so a replay of an old page does not re-publish.

use std::collections::BTreeMap;

use crate::ecosystem::Language;
use serde::Deserialize;

use super::catalog::{CatalogBatch, CatalogCursor, CatalogEvent};
use crate::upstream::{CatalogFollower, PollFuture, UpstreamClient, UpstreamError};

const DEFAULT_BASE: &str = "https://replicate.npmjs.com/registry";
const PAGE_LIMIT: u32 = 100;

/// One page of the npm changes feed, turned into catalog events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesPage {
    /// Events whose sequence is strictly after `since`.
    pub events: Vec<CatalogEvent>,
    /// `last_seq` from the page, or `since` when the page named none.
    pub last_seq: u64,
    /// No row on this page was strictly after `since`.
    pub exhausted: bool,
}

#[derive(Debug, Deserialize)]
struct ChangesBody {
    #[serde(default)]
    results: Vec<ChangeRow>,
    #[serde(default)]
    last_seq: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ChangeRow {
    seq: serde_json::Value,
    id: String,
    #[serde(default)]
    deleted: bool,
    doc: Option<PackageDoc>,
}

#[derive(Debug, Deserialize)]
struct PackageDoc {
    #[serde(default, rename = "dist-tags")]
    dist_tags: Option<DistTags>,
    #[serde(default)]
    versions: BTreeMap<String, VersionDoc>,
}

#[derive(Debug, Deserialize)]
struct VersionDoc {
    #[serde(default)]
    dependencies: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct DistTags {
    latest: Option<String>,
}

/// Parse one `_changes` body. `since` is the last sequence already applied.
pub fn parse_changes(body: &[u8], since: u64) -> Result<ChangesPage, UpstreamError> {
    let parsed: ChangesBody = serde_json::from_slice(body)
        .map_err(|err| UpstreamError::Parse(format!("npm _changes: {err}")))?;
    let mut events = Vec::new();
    let mut saw_newer = false;
    for row in parsed.results {
        let Some(seq) = seq_number(&row.seq) else {
            continue;
        };
        if seq <= since {
            continue;
        }
        saw_newer = true;
        if let Some(event) = event_from_row(&row) {
            events.push(event);
        }
    }
    let last_seq = parsed
        .last_seq
        .as_ref()
        .and_then(seq_number)
        .unwrap_or(since);
    Ok(ChangesPage {
        events,
        last_seq,
        exhausted: !saw_newer,
    })
}

fn event_from_row(row: &ChangeRow) -> Option<CatalogEvent> {
    if row.id.is_empty() || row.id.starts_with('_') {
        return None;
    }
    let latest = row
        .doc
        .as_ref()
        .and_then(|doc| doc.dist_tags.as_ref())
        .and_then(|tags| tags.latest.clone())
        .filter(|version| !version.is_empty());
    match (row.deleted, latest) {
        (true, Some(version)) => Some(CatalogEvent::Withdrawn {
            name: row.id.clone(),
            version,
        }),
        (false, Some(version)) => Some(CatalogEvent::Published {
            dependencies: dependency_names(row.doc.as_ref(), &version),
            name: row.id.clone(),
            version,
        }),
        _ => None,
    }
}

/// Dependency names declared on `version` inside the packument. Keys only:
/// the requirement string is not a name. Sorted so two parses of the same
/// document compare equal.
fn dependency_names(doc: Option<&PackageDoc>, version: &str) -> Vec<String> {
    let Some(doc) = doc else {
        return Vec::new();
    };
    let mut names: Vec<String> = doc
        .versions
        .get(version)
        .map(|body| body.dependencies.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn seq_number(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

fn cursor_seq(cursor: &CatalogCursor) -> u64 {
    match &cursor.0 {
        serde_json::Value::Number(number) => number.as_u64().unwrap_or(0),
        serde_json::Value::String(text) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

/// npm replicate changes follower.
pub struct NpmChangesFollower {
    base: String,
}

impl NpmChangesFollower {
    /// Custom base, for tests.
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    /// Production replicate registry.
    #[must_use]
    pub fn production() -> Self {
        Self::new(DEFAULT_BASE)
    }

    async fn poll_inner(
        &self,
        client: &UpstreamClient,
        cursor: &CatalogCursor,
    ) -> Result<CatalogBatch, UpstreamError> {
        let since = cursor_seq(cursor);
        let url = format!(
            "{}/_changes?since={since}&limit={PAGE_LIMIT}&include_docs=true",
            self.base
        );
        let bytes = client.get(Language::Typescript, &url).await?;
        let page = parse_changes(&bytes, since)?;
        Ok(CatalogBatch {
            events: page.events,
            next: CatalogCursor(serde_json::Value::from(page.last_seq)),
            exhausted: page.exhausted,
        })
    }
}

impl CatalogFollower for NpmChangesFollower {
    fn language(&self) -> Language {
        Language::Typescript
    }

    fn poll<'a>(&'a self, client: &'a UpstreamClient, cursor: &'a CatalogCursor) -> PollFuture<'a> {
        Box::pin(self.poll_inner(client, cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(json: &str) -> Vec<u8> {
        json.as_bytes().to_vec()
    }

    #[test]
    fn latest_publishes_and_delete_withdraws_that_version() {
        let page = parse_changes(
            &body(
                r#"{"results":[
                    {"seq":3,"id":"lodash","deleted":false,"doc":{"dist-tags":{"latest":"4.17.21"}}},
                    {"seq":4,"id":"left-pad","deleted":true,"doc":{"dist-tags":{"latest":"1.1.2"}}},
                    {"seq":5,"id":"gone","deleted":true},
                    {"seq":6,"id":"_design/app","doc":{"dist-tags":{"latest":"1.0.0"}}}
                ],"last_seq":6}"#,
            ),
            0,
        )
        .expect("page");
        assert_eq!(page.events, vec![
            CatalogEvent::Published {
                name: "lodash".into(),
                version: "4.17.21".into(),
                dependencies: Vec::new(),
            },
            CatalogEvent::Withdrawn {
                name: "left-pad".into(),
                version: "1.1.2".into(),
            },
        ]);
        assert_eq!(page.last_seq, 6);
        assert!(!page.exhausted);
    }

    #[test]
    fn published_event_keeps_dependency_names_from_the_latest_version() {
        let page = parse_changes(
            &body(
                r#"{"results":[
                    {"seq":1,"id":"left-pad","doc":{
                        "dist-tags":{"latest":"1.1.2"},
                        "versions":{
                            "1.0.0":{"dependencies":{"old":"1.0.0"}},
                            "1.1.2":{"dependencies":{"ms":"^2.0.0","debug":"4.0.0"}}
                        }
                    }}
                ],"last_seq":1}"#,
            ),
            0,
        )
        .expect("page");
        assert_eq!(page.events, vec![CatalogEvent::Published {
            name: "left-pad".into(),
            version: "1.1.2".into(),
            dependencies: vec!["debug".into(), "ms".into()],
        }]);
    }

    #[test]
    fn rows_at_or_below_since_do_not_replay() {
        let raw = body(
            r#"{"results":[
                {"seq":"2","id":"old","doc":{"dist-tags":{"latest":"1.0.0"}}},
                {"seq":3,"id":"@scope/pkg","doc":{"dist-tags":{"latest":"2.0.0"}}}
            ],"last_seq":"3"}"#,
        );
        let page = parse_changes(&raw, 2).expect("page");
        assert_eq!(page.events, vec![CatalogEvent::Published {
            name: "@scope/pkg".into(),
            version: "2.0.0".into(),
            dependencies: Vec::new(),
        }]);
        let again = parse_changes(&raw, 2).expect("replay");
        assert_eq!(page, again);
    }

    #[test]
    fn caught_up_page_is_exhausted_and_keeps_since() {
        let page = parse_changes(
            &body(r#"{"results":[{"seq":1,"id":"a","doc":{"dist-tags":{"latest":"1.0.0"}}}],"last_seq":1}"#),
            9,
        )
        .expect("page");
        assert!(page.events.is_empty());
        assert!(page.exhausted);
        assert_eq!(page.last_seq, 1);
    }
}
