//! Go module index follower (`index.golang.org/index`).
//!
//! The feed is newline-delimited JSON, oldest first, filtered by
//! `?since=<rfc3339>`. Each row is a module path and version. The index does
//! not carry retractions; those live in `go.mod` and are not events here.
//! A row at or before the cursor timestamp is not replayed. A line that is
//! not JSON, or that lacks a path or version, is dropped.

use crate::ecosystem::Language;
use serde::Deserialize;

use super::{
    catalog::{CatalogBatch, CatalogCursor, CatalogEvent, attach_document_dependencies},
    path_segment,
};
use crate::upstream::{CatalogFollower, PollFuture, UpstreamClient, UpstreamError};

const DEFAULT_BASE: &str = "https://index.golang.org";

/// One page of the module index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexPage {
    /// Rows strictly after `since`.
    pub events: Vec<CatalogEvent>,
    /// Greatest timestamp on the page, or `since` when none was newer.
    pub latest: String,
    /// No row was strictly after `since`.
    pub exhausted: bool,
}

#[derive(Debug, Deserialize)]
struct IndexRow {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Timestamp")]
    timestamp: String,
}

/// Parse an index body. `since` is the last timestamp already applied.
/// Empty `since` accepts every well-formed row.
pub fn parse_index(body: &[u8], since: &str) -> Result<IndexPage, UpstreamError> {
    let text = std::str::from_utf8(body)
        .map_err(|err| UpstreamError::Parse(format!("go index utf-8: {err}")))?;
    let mut events = Vec::new();
    let mut latest = since.to_owned();
    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<IndexRow>(line) else {
            continue;
        };
        if row.path.is_empty() || row.version.is_empty() || row.timestamp.is_empty() {
            continue;
        }
        if !since.is_empty() && row.timestamp.as_str() <= since {
            continue;
        }
        if row.timestamp > latest {
            latest.clone_from(&row.timestamp);
        }
        events.push(CatalogEvent::Published {
            name: row.path,
            version: row.version,
            dependencies: Vec::new(),
            checksum: None,
        });
    }
    Ok(IndexPage {
        exhausted: events.is_empty(),
        latest,
        events,
    })
}

fn cursor_since(cursor: &CatalogCursor) -> String {
    match &cursor.0 {
        serde_json::Value::String(text) => text.clone(),
        _ => String::new(),
    }
}

/// `index.golang.org` follower.
pub struct GoIndexFollower {
    base: String,
}

impl GoIndexFollower {
    /// Custom base, for tests.
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    /// Production module index.
    #[must_use]
    pub fn production() -> Self {
        Self::new(DEFAULT_BASE)
    }

    async fn poll_inner(
        &self,
        client: &UpstreamClient,
        cursor: &CatalogCursor,
    ) -> Result<CatalogBatch, UpstreamError> {
        let since = cursor_since(cursor);
        let url = if since.is_empty() {
            format!("{}/index", self.base)
        } else {
            format!("{}/index?since={since}", self.base)
        };
        let bytes = client.get(Language::Go, &url).await?;
        let mut page = parse_index(&bytes, &since)?;
        attach_document_dependencies(
            client,
            Language::Go,
            &mut page.events,
            |name, version| {
                let escaped = crate::ecosystem::escape_module_path(name);
                Some(format!(
                    "https://proxy.golang.org/{escaped}/@v/{}.mod",
                    path_segment(version)
                ))
            },
            |body| {
                std::str::from_utf8(body)
                    .map(crate::ecosystem::require_names)
                    .unwrap_or_default()
            },
        )
        .await;
        let next = if page.latest.is_empty() {
            cursor.clone()
        } else {
            CatalogCursor(serde_json::Value::String(page.latest))
        };
        Ok(CatalogBatch {
            events: page.events,
            next,
            exhausted: page.exhausted,
        })
    }
}

impl CatalogFollower for GoIndexFollower {
    fn language(&self) -> Language {
        Language::Go
    }

    fn poll<'a>(&'a self, client: &'a UpstreamClient, cursor: &'a CatalogCursor) -> PollFuture<'a> {
        Box::pin(self.poll_inner(client, cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_rows_after_the_cursor_and_skips_garbage() {
        let body = b"\n{\"Path\":\"golang.org/x/text\",\"Version\":\"v0.3.0\",\"Timestamp\":\"2020-01-01T00:00:00Z\"}\nnot-json\n{\"Path\":\"\",\"Version\":\"v1.0.0\",\"Timestamp\":\"2020-02-01T00:00:00Z\"}\n{\"Path\":\"rsc.io/quote\",\"Version\":\"v1.5.2\",\"Timestamp\":\"2020-03-01T00:00:00Z\"}\n";
        let page = parse_index(body, "2020-01-01T00:00:00Z").expect("index");
        assert_eq!(page.events, vec![CatalogEvent::Published {
            name: "rsc.io/quote".into(),
            version: "v1.5.2".into(),
            dependencies: Vec::new(),
            checksum: None,
        }]);
        assert_eq!(page.latest, "2020-03-01T00:00:00Z");
        assert!(!page.exhausted);
        let again = parse_index(body, "2020-01-01T00:00:00Z").expect("replay");
        assert_eq!(page, again);
    }

    #[test]
    fn caught_up_index_is_exhausted() {
        let body =
            b"{\"Path\":\"rsc.io/quote\",\"Version\":\"v1.5.2\",\"Timestamp\":\"2020-03-01T00:00:00Z\"}\n";
        let page = parse_index(body, "2020-03-01T00:00:00Z").expect("index");
        assert!(page.events.is_empty());
        assert!(page.exhausted);
        assert_eq!(page.latest, "2020-03-01T00:00:00Z");
    }
}
