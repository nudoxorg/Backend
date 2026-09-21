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
//!
//! The field does not demand `pkg:`. A reader types `serde`, or `serde@1.0`,
//! picks an ecosystem from a row of toggles, and this module resolves the
//! rest from what the catalog actually recorded: no version means the newest
//! one; a version the catalog does not hold means the closest one it does,
//! and the flow says which was chosen and why. The rules live in
//! [`resolve`] as a pure function so they are provable without a service.

use super::events::CatalogEvent;
use super::service::{Endpoint, Outcome, Request};
use backend_library::{
    PackageReference, ProductText, RegistryPackageRecord, SurfaceCommand, SurfaceReply,
};
use backend_present::{Fault, ProductView, product_view};
use gpui::AppContext as _;
use gpui::{Context, EventEmitter, Task};
use std::time::Duration;

/// How long the field waits after a keystroke before asking the catalog.
const DEBOUNCE: Duration = Duration::from_millis(180);

/// How many suggestions one lookup asks for.
///
/// Wide enough to hold every recorded version of one name, since the same
/// rows resolve a typed version to the nearest recorded one.
const LIMIT: u16 = 64;

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

    /// Builds one row without a service, for the resolution tests.
    #[cfg(test)]
    pub(crate) fn for_test(
        coordinate: String,
        name: &str,
        version: &str,
        ecosystem: &str,
    ) -> Self {
        Self {
            coordinate,
            name: name.to_owned(),
            version: version.to_owned(),
            ecosystem: ecosystem.to_owned(),
            bytes: 0,
        }
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

/// What a reader typed into the add field, taken apart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Ask {
    /// An absolute folder on this machine.
    Folder(String),
    /// A complete pinned package URL, taken as spelled.
    Pinned(String),
    /// A registry name, with an optional version the reader wanted.
    Named {
        /// Registry-native package name.
        name: String,
        /// The version asked for, when one was typed.
        version: Option<String>,
    },
}

impl Ask {
    /// Splits field text into what it asks for.
    ///
    /// `pkg:…` is taken whole; a leading `/` or `~` is a folder; anything
    /// else is a name with an optional `@version` or a trailing space and
    /// version, so `serde 1.0.200` and `serde@1.0.200` both work.
    pub(crate) fn parse(text: &str) -> Self {
        let trimmed = text.trim();
        if trimmed.starts_with("pkg:") {
            return Self::Pinned(trimmed.to_owned());
        }
        if trimmed.starts_with('/') || trimmed.starts_with('~') {
            return Self::Folder(trimmed.to_owned());
        }
        let (name, version) = trimmed
            .rsplit_once('@')
            .filter(|(name, _)| !name.is_empty())
            .or_else(|| trimmed.split_once(char::is_whitespace))
            .map_or((trimmed, None), |(name, version)| {
                (name.trim(), Some(version.trim()).filter(|v| !v.is_empty()))
            });
        Self::Named {
            name: name.to_owned(),
            version: version.map(ToOwned::to_owned),
        }
    }

}

/// What resolving a named ask against the catalog decided.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Resolution {
    /// The exact version asked for, or the newest when none was asked for.
    Exact {
        /// The pinned coordinate to index.
        coordinate: String,
    },
    /// The version asked for is not recorded; the nearest recorded one is.
    Nearest {
        /// The pinned coordinate to index.
        coordinate: String,
        /// The version the reader typed.
        wanted: String,
        /// The version chosen instead.
        chosen: String,
    },
    /// The catalog records nothing under this name in this ecosystem.
    Unknown {
        /// The name the reader typed.
        name: String,
        /// The ecosystem token it was looked for in.
        ecosystem: String,
    },
}

impl Resolution {
    /// Returns the coordinate to index, when one was decided.
    pub(crate) fn coordinate(&self) -> Option<&str> {
        match self {
            Self::Exact { coordinate } | Self::Nearest { coordinate, .. } => Some(coordinate),
            Self::Unknown { .. } => None,
        }
    }

    /// Returns the sentence the flow shows for this decision, if any.
    pub(crate) fn notice(&self) -> Option<String> {
        match self {
            Self::Exact { .. } => None,
            Self::Nearest { wanted, chosen, .. } => Some(format!(
                "{wanted} is not in the local index; indexing {chosen}, the closest recorded version."
            )),
            Self::Unknown { name, ecosystem } => Some(format!(
                "The local index records no {ecosystem} package named {name}."
            )),
        }
    }
}

/// Resolves a name and optional version against the catalog rows for it.
///
/// Rows are the catalog's answer for the name; only exact-name rows in the
/// chosen ecosystem count. With no version the newest wins. With a version,
/// an exact match wins; otherwise the newest version sharing the longest
/// dotted prefix with the wanted one, and failing that the newest overall.
pub(crate) fn resolve(
    rows: &[Suggestion],
    ecosystem: &str,
    name: &str,
    wanted: Option<&str>,
) -> Resolution {
    let mut candidates: Vec<&Suggestion> = rows
        .iter()
        .filter(|row| row.ecosystem() == ecosystem && row.name().eq_ignore_ascii_case(name))
        .collect();
    candidates.sort_by_key(|row| super::registry::version_rank(row.version()));
    let Some(newest) = candidates.first() else {
        return Resolution::Unknown {
            name: name.to_owned(),
            ecosystem: ecosystem.to_owned(),
        };
    };
    let Some(wanted) = wanted else {
        return Resolution::Exact {
            coordinate: newest.coordinate().to_owned(),
        };
    };
    if let Some(exact) = candidates.iter().find(|row| row.version() == wanted) {
        return Resolution::Exact {
            coordinate: exact.coordinate().to_owned(),
        };
    }
    let closest = candidates
        .iter()
        .max_by_key(|row| {
            (
                shared_prefix(row.version(), wanted),
                std::cmp::Reverse(super::registry::version_rank(row.version())),
            )
        })
        .filter(|row| shared_prefix(row.version(), wanted) > 0)
        .unwrap_or(newest);
    Resolution::Nearest {
        coordinate: closest.coordinate().to_owned(),
        wanted: wanted.to_owned(),
        chosen: closest.version().to_owned(),
    }
}

/// Returns how many leading dotted components two versions share.
fn shared_prefix(left: &str, right: &str) -> usize {
    left.split('.')
        .zip(right.split('.'))
        .take_while(|(a, b)| a == b)
        .count()
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

/// One named section of registry facts about one package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Facts {
    coordinate: String,
    sections: Vec<(&'static str, ProductView)>,
}

impl Facts {
    /// Returns each section and the view the engine answered with.
    pub(crate) fn sections(&self) -> &[(&'static str, ProductView)] {
        &self.sections
    }
}

/// The registry catalog behind the add-a-project field and the project page.
pub(crate) struct CatalogStore {
    endpoint: Endpoint,
    state: Catalog,
    facts: Option<Facts>,
    pending: Option<Task<()>>,
    facts_task: Option<Task<()>>,
    resolving: Option<Task<()>>,
    resolved: Option<Resolution>,
    generation: u64,
}

impl EventEmitter<CatalogEvent> for CatalogStore {}

impl CatalogStore {
    /// Creates an idle catalog bound to one endpoint.
    pub(crate) const fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            state: Catalog::Idle,
            facts: None,
            pending: None,
            facts_task: None,
            resolving: None,
            resolved: None,
            generation: 0,
        }
    }

    /// Returns what the catalog last said about the add field.
    pub(crate) const fn state(&self) -> &Catalog {
        &self.state
    }

    /// Returns the registry facts last read for one package, if they match.
    pub(crate) fn facts_for(&self, coordinate: &str) -> Option<&Facts> {
        self.facts
            .as_ref()
            .filter(|facts| facts.coordinate == coordinate)
    }

    /// Reads the registry sections one pinned package coordinate has.
    ///
    /// Only a pinned package has them. A local folder is not in any registry,
    /// and the page says exactly that rather than drawing three empty
    /// sections that look like a failure.
    pub(crate) fn read_facts(&mut self, coordinate: &str, cx: &mut Context<Self>) {
        if self.facts_for(coordinate).is_some() {
            return;
        }
        let Ok(package) = PackageReference::parse(coordinate) else {
            self.facts = Some(Facts {
                coordinate: coordinate.to_owned(),
                sections: Vec::new(),
            });
            cx.notify();
            return;
        };
        let endpoint = self.endpoint.clone();
        let wanted = coordinate.to_owned();
        self.facts_task = Some(cx.spawn(async move |this, cx| {
            let sections = cx
                .background_spawn(async move { read_sections(&endpoint, &package) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.facts = Some(Facts {
                    coordinate: wanted,
                    sections,
                });
                cx.emit(CatalogEvent::Changed);
                cx.notify();
            });
        }));
    }

    /// Forgets the last lookup, for when the flow closes.
    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        self.resolving = None;
        self.state = Catalog::Idle;
        cx.notify();
    }

    /// Takes the last resolution, once; the window acts on it exactly once.
    pub(crate) fn take_resolved(&mut self) -> Option<Resolution> {
        self.resolved.take()
    }

    /// Resolves a typed name and optional version against a fresh lookup.
    ///
    /// The field's own lookup may still be debouncing, so this asks the
    /// catalog again, unbounced, and emits [`CatalogEvent::Resolved`] when the
    /// decision is in. An empty or inadmissible name resolves to unknown at
    /// once rather than asking the service a question it would refuse.
    pub(crate) fn resolve_named(
        &mut self,
        ecosystem: &str,
        name: &str,
        version: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Ok(query) = ProductText::new(name) else {
            self.resolved = Some(Resolution::Unknown {
                name: name.to_owned(),
                ecosystem: ecosystem.to_owned(),
            });
            cx.emit(CatalogEvent::Resolved);
            return;
        };
        let request = Request::Surface {
            command: Box::new(SurfaceCommand::IndexSearch { query, limit: LIMIT }),
        };
        let endpoint = self.endpoint.clone();
        let (ecosystem, name, version) =
            (ecosystem.to_owned(), name.to_owned(), version.map(ToOwned::to_owned));
        self.resolving = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move { super::service::run(endpoint.path(), &request) })
                .await;
            let rows = match outcome {
                Ok(Outcome::Surface(reply)) => rows_of(&reply),
                _ => Vec::new(),
            };
            let _ = this.update(cx, |this, cx| {
                this.resolved = Some(resolve(&rows, &ecosystem, &name, version.as_deref()));
                cx.emit(CatalogEvent::Resolved);
                cx.notify();
            });
        }));
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
    let needle = match Ask::parse(text) {
        Ask::Folder(_) => return None,
        Ask::Pinned(pinned) => pinned
            .rsplit('/')
            .next()
            .and_then(|tail| tail.split('@').next())
            .unwrap_or_default()
            .to_owned(),
        Ask::Named { name, .. } => name,
    };
    if needle.is_empty() {
        return Some(SurfaceCommand::Explore {
            query: None,
            limit: LIMIT,
        });
    }
    ProductText::new(&needle)
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

/// The three registry sections a package page can carry, in reading order.
fn read_sections(
    endpoint: &Endpoint,
    package: &PackageReference,
) -> Vec<(&'static str, ProductView)> {
    [
        ("Versions", SurfaceCommand::PackageVersions {
            package: package.clone(),
        }),
        ("Dependencies", SurfaceCommand::Dependencies {
            package: package.clone(),
        }),
        ("Dependents", SurfaceCommand::Dependents {
            package: package.clone(),
        }),
        ("Registry", SurfaceCommand::Package {
            package: package.clone(),
        }),
    ]
    .into_iter()
    .filter_map(|(title, command)| section(endpoint, title, command))
    .collect()
}

fn section(
    endpoint: &Endpoint,
    title: &'static str,
    command: SurfaceCommand,
) -> Option<(&'static str, ProductView)> {
    let request = Request::Surface {
        command: Box::new(command),
    };
    match super::service::run(endpoint.path(), &request) {
        Ok(Outcome::Surface(reply)) => Some((title, product_view(&reply))),
        _ => None,
    }
}
