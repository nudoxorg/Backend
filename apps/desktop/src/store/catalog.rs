//! The local registry catalog, as the add-a-project flow sees it.
//!
//! Typing a package coordinate from memory is the one part of adding a project
//! that a reader cannot be expected to get right: the ecosystem prefix, the
//! exact registry name, and a pinned version all have to be correct before the
//! engine will accept it. So the field asks. Every keystroke runs the same
//! `index-search` the CLI runs — `explore` when the field is empty — and the
//! rows that come back are real catalog records, each one a coordinate the
//! engine has already admitted.
//!
//! Nothing here invents a suggestion. If the local catalog holds nothing, the
//! flow says so and the reader types a coordinate, which still works.

use super::events::CatalogEvent;
use super::service::{Endpoint, Outcome, Request};
use backend_library::{ProductText, RegistryPackageRecord, SurfaceCommand, SurfaceReply};
use backend_present::Fault;
use gpui::AppContext as _;
use gpui::{Context, EventEmitter, Task};
use std::time::Duration;

/// How long the field waits after a keystroke before asking the catalog.
const DEBOUNCE: Duration = Duration::from_millis(180);

/// How many suggestions one lookup asks for.
const LIMIT: u16 = 8;

/// One catalog row the reader can accept with a click.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Suggestion {
    coordinate: String,
    name: String,
    version: String,
    ecosystem: String,
    bytes: u64,
}

impl Suggestion {
    /// Returns the exact pinned coordinate this row submits.
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Returns the registry-native package name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the immutable version.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Returns the closed ecosystem spelling.
    pub(crate) fn ecosystem(&self) -> &str {
        &self.ecosystem
    }

    /// Returns the verified archive size in bytes.
    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }

    fn of(record: &RegistryPackageRecord) -> Self {
        Self {
            coordinate: record.coordinate.as_str().to_owned(),
            name: record.name.as_str().to_owned(),
            version: record.version.as_str().to_owned(),
            ecosystem: format!("{:?}", record.ecosystem).to_ascii_lowercase(),
            bytes: record.bytes,
        }
    }
}

/// What the catalog said about the text in the field.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Catalog {
    /// No lookup has run for this text yet.
    #[default]
    Idle,
    /// A lookup is on the wire.
    Looking,
    /// The catalog answered with these rows; an empty slice means it holds none.
    Rows(Vec<Suggestion>),
    /// The lookup failed.
    Faulted(Box<Fault>),
}

/// The registry catalog behind the add-a-project field.
pub(crate) struct CatalogStore {
    endpoint: Endpoint,
    state: Catalog,
    pending: Option<Task<()>>,
    generation: u64,
}

impl EventEmitter<CatalogEvent> for CatalogStore {}

impl CatalogStore {
    /// Creates an idle catalog bound to one endpoint.
    pub(crate) const fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            state: Catalog::Idle,
            pending: None,
            generation: 0,
        }
    }

    /// Returns what the catalog last said.
    pub(crate) const fn state(&self) -> &Catalog {
        &self.state
    }

    /// Forgets the last lookup, for when the flow closes.
    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        self.state = Catalog::Idle;
        cx.notify();
    }

    /// Looks the field text up in the local catalog, debounced.
    pub(crate) fn look_up(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(command) = command_for(text) else {
            self.pending = None;
            self.state = Catalog::Idle;
            cx.notify();
            return;
        };
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        let endpoint = self.endpoint.clone();
        self.state = Catalog::Looking;
        cx.notify();
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let request = Request::Surface {
                command: Box::new(command),
            };
            let outcome = cx
                .background_spawn(async move { super::service::run(endpoint.path(), &request) })
                .await;
            let _ = this.update(cx, |this, cx| this.install(generation, outcome, cx));
        }));
    }

    fn install(
        &mut self,
        generation: u64,
        outcome: Result<Outcome, backend_client::ClientError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        self.state = match outcome {
            Ok(Outcome::Surface(reply)) => Catalog::Rows(rows_of(&reply)),
            Ok(_) => Catalog::Rows(Vec::new()),
            Err(error) => Catalog::Faulted(Box::new(Fault::from_client_error(
                &error,
                backend_present::Operand::Argument("index-search".to_owned()),
            ))),
        };
        cx.emit(CatalogEvent::Changed);
        cx.notify();
    }
}

/// Returns the surface command one field text should run, when it runs one.
///
/// A path is never looked up: the filesystem, not the registry, is the
/// authority on whether a folder exists, and the field validates that itself.
fn command_for(text: &str) -> Option<SurfaceCommand> {
    let trimmed = text.trim();
    if trimmed.starts_with('/') || trimmed.starts_with('~') {
        return None;
    }
    let needle = trimmed.strip_prefix("pkg:").unwrap_or(trimmed);
    let needle = needle
        .rsplit('/')
        .next()
        .unwrap_or(needle)
        .split('@')
        .next()
        .unwrap_or(needle);
    if needle.is_empty() {
        return Some(SurfaceCommand::Explore {
            query: None,
            limit: LIMIT,
        });
    }
    ProductText::new(needle)
        .ok()
        .map(|query| SurfaceCommand::IndexSearch {
            query,
            limit: LIMIT,
        })
}

fn rows_of(reply: &SurfaceReply) -> Vec<Suggestion> {
    match reply {
        SurfaceReply::IndexSearch(records) | SurfaceReply::Explored(records) => {
            records.iter().map(Suggestion::of).collect()
        }
        _ => Vec::new(),
    }
}
