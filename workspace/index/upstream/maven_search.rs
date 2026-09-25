//! Maven Central search follower.
//!
//! `search.maven.org` returns GAV rows with a millisecond `timestamp`.
//! The catalog name is `groupId:artifactId`, matching Java's canonical
//! render. A row at or before the cursor timestamp is not replayed. A doc
//! missing group, artifact, or version is dropped. Maven Central has no yank
//! flag on this feed, so every kept row is [`CatalogEvent::Published`].

use crate::ecosystem::Language;
use serde::Deserialize;

use super::{
    catalog::{CatalogBatch, CatalogCursor, CatalogEvent, attach_document_dependencies},
    path_segment,
};
use crate::upstream::{CatalogFollower, PollFuture, UpstreamClient, UpstreamError};

const DEFAULT_BASE: &str = "https://search.maven.org/solrsearch/select";
const PAGE_ROWS: u32 = 100;

/// One search page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    /// Docs strictly after `since_ms`.
    pub events: Vec<CatalogEvent>,
    /// Greatest timestamp on the page, or `since_ms` when none was newer.
    pub latest_ms: i64,
    /// No doc was strictly after `since_ms`.
    pub exhausted: bool,
}

#[derive(Debug, Deserialize)]
struct SearchBody {
    response: Option<SearchResponse>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    docs: Vec<GavDoc>,
}

#[derive(Debug, Deserialize)]
struct GavDoc {
    #[serde(default)]
    g: String,
    #[serde(default)]
    a: String,
    #[serde(default)]
    v: String,
    #[serde(default)]
    timestamp: i64,
}

/// Parse a Solr JSON body. `since_ms` is the last timestamp already applied.
pub fn parse_search(body: &[u8], since_ms: i64) -> Result<SearchPage, UpstreamError> {
    let parsed: SearchBody = serde_json::from_slice(body)
        .map_err(|err| UpstreamError::Parse(format!("maven search: {err}")))?;
    let mut events = Vec::new();
    let mut latest_ms = since_ms;
    for doc in parsed
        .response
        .map(|response| response.docs)
        .unwrap_or_default()
    {
        if doc.g.is_empty() || doc.a.is_empty() || doc.v.is_empty() {
            continue;
        }
        if doc.timestamp <= since_ms {
            continue;
        }
        if doc.timestamp > latest_ms {
            latest_ms = doc.timestamp;
        }
        events.push(CatalogEvent::Published {
            name: format!("{}:{}", doc.g, doc.a),
            version: doc.v,
            dependencies: Vec::new(),
        });
    }
    Ok(SearchPage {
        exhausted: events.is_empty(),
        latest_ms,
        events,
    })
}

/// Maven Central POM for a `group:artifact` coordinate.
///
/// Dots in the group become path separators before each piece is encoded.
/// `None` when the name is not `group:artifact` or any piece is empty.
#[must_use]
pub fn pom_url(name: &str, version: &str) -> Option<String> {
    let (group, artifact) = name.split_once(':')?;
    if group.is_empty() || artifact.is_empty() || version.is_empty() {
        return None;
    }
    let group_path = group
        .split('.')
        .map(path_segment)
        .collect::<Vec<_>>()
        .join("/");
    let artifact = path_segment(artifact);
    let version = path_segment(version);
    Some(format!(
        "https://repo1.maven.org/maven2/{group_path}/{artifact}/{version}/{artifact}-{version}.pom"
    ))
}

fn cursor_ms(cursor: &CatalogCursor) -> i64 {
    match &cursor.0 {
        serde_json::Value::Number(number) => number.as_i64().unwrap_or(0),
        serde_json::Value::String(text) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Maven Central `solrsearch` follower.
pub struct MavenSearchFollower {
    base: String,
}

impl MavenSearchFollower {
    /// Custom base, for tests.
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    /// Production search endpoint.
    #[must_use]
    pub fn production() -> Self {
        Self::new(DEFAULT_BASE)
    }

    async fn poll_inner(
        &self,
        client: &UpstreamClient,
        cursor: &CatalogCursor,
    ) -> Result<CatalogBatch, UpstreamError> {
        let since_ms = cursor_ms(cursor);
        let url = format!(
            "{}?q=timestamp:[{}+TO+*]&rows={PAGE_ROWS}&wt=json&core=gav",
            self.base, since_ms
        );
        let bytes = client.get(Language::Java, &url).await?;
        let mut page = parse_search(&bytes, since_ms)?;
        attach_document_dependencies(
            client,
            Language::Java,
            &mut page.events,
            pom_url,
            crate::ecosystem::pom_dependency_names,
        )
        .await;
        Ok(CatalogBatch {
            events: page.events,
            next: CatalogCursor(serde_json::Value::from(page.latest_ms)),
            exhausted: page.exhausted,
        })
    }
}

impl CatalogFollower for MavenSearchFollower {
    fn language(&self) -> Language {
        Language::Java
    }

    fn poll<'a>(&'a self, client: &'a UpstreamClient, cursor: &'a CatalogCursor) -> PollFuture<'a> {
        Box::pin(self.poll_inner(client, cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gav_after_the_cursor_publishes_and_incomplete_docs_drop() {
        let body = br#"{"response":{"docs":[
            {"g":"org.slf4j","a":"slf4j-api","v":"1.7.36","timestamp":100},
            {"g":"org.slf4j","a":"slf4j-api","v":"2.0.9","timestamp":200},
            {"g":"","a":"x","v":"1","timestamp":300},
            {"g":"com.example","a":"lib","v":"","timestamp":400}
        ]}}"#;
        let page = parse_search(body, 100).expect("search");
        assert_eq!(page.events, vec![CatalogEvent::Published {
            name: "org.slf4j:slf4j-api".into(),
            version: "2.0.9".into(),
            dependencies: Vec::new(),
        }]);
        assert_eq!(page.latest_ms, 200);
        assert!(!page.exhausted);
        assert_eq!(parse_search(body, 100).expect("replay"), page);
    }

    #[test]
    fn caught_up_search_is_exhausted() {
        let body =
            br#"{"response":{"docs":[{"g":"org.slf4j","a":"slf4j-api","v":"2.0.9","timestamp":200}]}}"#;
        let page = parse_search(body, 200).expect("search");
        assert!(page.events.is_empty());
        assert!(page.exhausted);
        assert_eq!(page.latest_ms, 200);
    }

    #[test]
    fn pom_url_splits_the_group_and_encodes_each_segment() {
        assert_eq!(
            pom_url("org.slf4j:slf4j-api", "2.0.9").as_deref(),
            Some("https://repo1.maven.org/maven2/org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.pom")
        );
        assert_eq!(
            pom_url("com.example:lib+extra", "1.0").as_deref(),
            Some("https://repo1.maven.org/maven2/com/example/lib%2Bextra/1.0/lib%2Bextra-1.0.pom")
        );
        assert!(pom_url("nosplit", "1.0").is_none());
        assert!(pom_url("g:", "1.0").is_none());
    }
}
