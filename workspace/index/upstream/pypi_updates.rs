//! PyPI updates-feed follower (`pypi.org/rss/updates.xml`).
//!
//! Each `<item>` title is `name version`. The cursor is the item's `pubDate`
//! rewritten as `YYYY-MM-DDTHH:MM:SS` so order does not depend on the weekday
//! token. An item at or before that cursor is not replayed. A title with no
//! version token, or a `pubDate` that does not parse, is dropped. The feed
//! does not say whether a release is yanked, so every kept row is
//! [`CatalogEvent::Published`].

use crate::ecosystem::Language;

use super::catalog::{CatalogBatch, CatalogCursor, CatalogEvent};
use crate::upstream::{CatalogFollower, PollFuture, UpstreamClient, UpstreamError};

const DEFAULT_URL: &str = "https://pypi.org/rss/updates.xml";

/// One updates page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatesPage {
    /// Items strictly after `since`.
    pub events: Vec<CatalogEvent>,
    /// Greatest sortable timestamp on the page, or `since` when none was newer.
    pub latest: String,
    /// No item was strictly after `since`.
    pub exhausted: bool,
}

/// Parse an updates RSS body. `since` is `YYYY-MM-DDTHH:MM:SS` or empty.
pub fn parse_updates(body: &[u8], since: &str) -> Result<UpdatesPage, UpstreamError> {
    let mut reader = quick_xml::Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_item = false;
    let mut field: Option<Field> = None;
    let mut title = String::new();
    let mut published = String::new();
    let mut events = Vec::new();
    let mut latest = since.to_owned();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(tag)) => {
                let name = String::from_utf8_lossy(tag.name().as_ref()).into_owned();
                match name.as_str() {
                    "item" => {
                        in_item = true;
                        title.clear();
                        published.clear();
                    }
                    "title" if in_item => field = Some(Field::Title),
                    "pubDate" if in_item => field = Some(Field::Published),
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(text)) => {
                let Ok(decoded) = text.unescape() else {
                    continue;
                };
                match field {
                    Some(Field::Title) => title.push_str(&decoded),
                    Some(Field::Published) => published.push_str(&decoded),
                    None => {}
                }
            }
            Ok(quick_xml::events::Event::End(tag)) => {
                let name = String::from_utf8_lossy(tag.name().as_ref()).into_owned();
                match name.as_str() {
                    "title" | "pubDate" => field = None,
                    "item" => {
                        in_item = false;
                        if let Some((event, stamp)) = event_from_item(&title, &published, since) {
                            if stamp > latest {
                                latest.clone_from(&stamp);
                            }
                            events.push(event);
                        }
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(err) => return Err(UpstreamError::Parse(format!("pypi updates: {err}"))),
            _ => {}
        }
        buf.clear();
    }

    Ok(UpdatesPage {
        exhausted: events.is_empty(),
        latest,
        events,
    })
}

enum Field {
    Title,
    Published,
}

fn event_from_item(title: &str, published: &str, since: &str) -> Option<(CatalogEvent, String)> {
    let stamp = sortable_pubdate(published)?;
    if !since.is_empty() && stamp.as_str() <= since {
        return None;
    }
    let (name, version) = title.rsplit_once(' ')?;
    let name = name.trim();
    let version = version.trim();
    if name.is_empty() || version.is_empty() {
        return None;
    }
    Some((
        CatalogEvent::Published {
            name: name.to_owned(),
            version: version.to_owned(),
            dependencies: Vec::new(),
        },
        stamp,
    ))
}

/// `Fri, 01 Jan 2020 00:00:00 GMT` → `2020-01-01T00:00:00`.
fn sortable_pubdate(raw: &str) -> Option<String> {
    let mut parts = raw.split_whitespace();
    let day = if raw.contains(',') {
        parts.next()?;
        parts.next()?
    } else {
        parts.next()?
    };
    let month = month_number(parts.next()?)?;
    let year = parts.next()?;
    let time = parts.next()?;
    if year.len() != 4 || day.is_empty() || time.len() < 8 {
        return None;
    }
    let day: u8 = day.parse().ok()?;
    if !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{year}-{month:02}-{day:02}T{time}"))
}

fn month_number(name: &str) -> Option<u8> {
    Some(match name {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

fn cursor_since(cursor: &CatalogCursor) -> String {
    match &cursor.0 {
        serde_json::Value::String(text) => text.clone(),
        _ => String::new(),
    }
}

/// PyPI updates RSS follower.
pub struct PypiUpdatesFollower {
    url: String,
}

impl PypiUpdatesFollower {
    /// Custom feed URL, for tests.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    /// Production updates feed.
    #[must_use]
    pub fn production() -> Self {
        Self::new(DEFAULT_URL)
    }

    async fn poll_inner(
        &self,
        client: &UpstreamClient,
        cursor: &CatalogCursor,
    ) -> Result<CatalogBatch, UpstreamError> {
        let since = cursor_since(cursor);
        let bytes = client.get(Language::Python, &self.url).await?;
        let page = parse_updates(&bytes, &since)?;
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

impl CatalogFollower for PypiUpdatesFollower {
    fn language(&self) -> Language {
        Language::Python
    }

    fn poll<'a>(&'a self, client: &'a UpstreamClient, cursor: &'a CatalogCursor) -> PollFuture<'a> {
        Box::pin(self.poll_inner(client, cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(items: &str) -> Vec<u8> {
        format!("<rss><channel>{items}</channel></rss>").into_bytes()
    }

    #[test]
    fn title_publishes_and_older_pubdate_is_not_replayed() {
        let body = feed(
            r#"
            <item><title>requests 2.31.0</title><pubDate>Wed, 01 Jan 2020 00:00:00 GMT</pubDate></item>
            <item><title>only-a-name</title><pubDate>Thu, 02 Jan 2020 00:00:00 GMT</pubDate></item>
            <item><title>zope.interface 6.0</title><pubDate>Fri, 03 Jan 2020 12:30:00 GMT</pubDate></item>
            <item><title>bad date 1.0</title><pubDate>not-a-date</pubDate></item>
            "#,
        );
        let page = parse_updates(&body, "2020-01-01T00:00:00").expect("updates");
        assert_eq!(page.events, vec![CatalogEvent::Published {
            name: "zope.interface".into(),
            version: "6.0".into(),
            dependencies: Vec::new(),
        }]);
        assert_eq!(page.latest, "2020-01-03T12:30:00");
        assert_eq!(
            parse_updates(&body, "2020-01-01T00:00:00").expect("replay"),
            page
        );
    }

    #[test]
    fn caught_up_feed_is_exhausted() {
        let body = feed(
            r#"<item><title>requests 2.31.0</title><pubDate>Wed, 01 Jan 2020 00:00:00 GMT</pubDate></item>"#,
        );
        let page = parse_updates(&body, "2020-01-01T00:00:00").expect("updates");
        assert!(page.events.is_empty());
        assert!(page.exhausted);
        assert_eq!(page.latest, "2020-01-01T00:00:00");
    }
}
