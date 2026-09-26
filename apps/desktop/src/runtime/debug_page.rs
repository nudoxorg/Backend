//! Temporary debug page: every loaded read model as plain text.
//!
//! This is the lead's window onto the data plane while the boards are being
//! built: no design, just the facts each read model carries and the typed gap
//! behind every field it could not fill. It renders through the same store a
//! board will use. Later lanes delete it.

use super::store::{DataStore, StoreEvent};
use crate::core::{Activity, Resource, ResourceTerminal};
use crate::model::pages::{
    DocFragment, Gap, HealthModel, Known, OrbitModel, PackageDossier, PageKey, Provenance,
    Relation, RelationKind, SearchPage, SourceView, SymbolPage, confidence_name, link_name,
};
use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, div, px,
};
use std::fmt::Write as _;

fn gap(gap: &Gap) -> String {
    format!("∅ {gap}")
}

fn known<T>(value: &Known<T>, show: impl FnOnce(&T) -> String) -> String {
    match value {
        Known::Known(value) => show(value),
        Known::Unknown(missing) => gap(missing),
    }
}

fn status<T>(resource: &Resource<T>) -> String {
    let activity = match resource.activity() {
        Activity::Rest => "rest",
        Activity::Working => "working",
        Activity::Waiting => "waiting",
        Activity::Stopped => "stopped",
        Activity::NotYet => "not-yet",
    };
    let terminal = match resource.terminal() {
        ResourceTerminal::Complete => "complete".to_owned(),
        ResourceTerminal::Unavailable(reason) => format!("unavailable({reason:?})"),
        ResourceTerminal::Fault(error) => format!("fault({:?}: {})", error.code(), error.message()),
    };
    format!("[{activity} · {terminal}]")
}

fn relation(relation: &Relation) -> String {
    let kind = match relation.kind {
        RelationKind::Semantic(kind) => link_name(kind),
        RelationKind::Contains => "contains",
    };
    let provenance = match relation.provenance {
        Provenance::Derived { via, confidence } => {
            format!("derived:{via:?}/{}", confidence_name(confidence))
        }
        other => other.name().to_owned(),
    };
    let via = relation
        .via
        .as_ref()
        .map(|via| format!(" via {}", via.coordinate))
        .unwrap_or_default();
    format!(
        "{} {} ({}, {kind}, {provenance}, arrival {:?}){via}",
        relation.decl.kind_name(),
        relation.decl.name,
        relation.decl.coordinate,
        relation.arrival
    )
}

fn relations(out: &mut String, label: &str, value: &Known<std::sync::Arc<[Relation]>>) {
    match value {
        Known::Known(items) if items.is_empty() => {
            let _ = writeln!(out, "  {label}: none");
        }
        Known::Known(items) => {
            let _ = writeln!(out, "  {label}:");
            for item in items.iter() {
                let _ = writeln!(out, "    - {}", relation(item));
            }
        }
        Known::Unknown(missing) => {
            let _ = writeln!(out, "  {label}: {}", gap(missing));
        }
    }
}

/// Renders one declaration page.
#[must_use]
#[allow(clippy::too_many_lines)] // a flat field-by-field dump
pub fn symbol_text(page: &SymbolPage) -> String {
    let mut out = String::new();
    let id = &page.identity;
    let _ = writeln!(
        out,
        "{} {} [{} · {}]",
        id.kind_name(),
        id.name,
        id.family.name(),
        format!("{:?}", id.language).to_lowercase()
    );
    let _ = writeln!(out, "  coordinate: {}", id.coordinate);
    let _ = writeln!(out, "  package: {}", known(&page.package, ToString::to_string));
    let _ = writeln!(
        out,
        "  signature: {}",
        known(&page.signature, |signature| {
            let links = signature
                .links()
                .filter_map(|token| {
                    token.link.as_ref().map(|link| {
                        format!(
                            "{}→{} ({})",
                            signature.token_text(token),
                            link.target,
                            link.provenance.name()
                        )
                    })
                })
                .collect::<Vec<_>>();
            if links.is_empty() {
                signature.text.to_string()
            } else {
                format!("{}   links: {}", signature.text, links.join(", "))
            }
        })
    );
    let docs = DocFragment::plain_text(&page.docs);
    let _ = writeln!(
        out,
        "  docs: {}",
        if docs.trim().is_empty() {
            "(the producer captured none)".to_owned()
        } else {
            docs.replace('\n', "\n        ")
        }
    );
    let _ = writeln!(
        out,
        "  site: {} · excerpt {}",
        known(&page.site.location, |location| format!("{}:{}", location.path, location.line)),
        known(&page.site.excerpt, |excerpt| format!(
            "{} bytes{}",
            excerpt.text.len(),
            if excerpt.complete { "" } else { " (truncated)" }
        ))
    );
    match &page.members {
        Known::Known(members) => {
            let _ = writeln!(out, "  members ({}):", members.len());
            for member in members.made_of.iter() {
                let _ = writeln!(out, "    made of: {} {}", member.decl.kind_name(), member.decl.name);
            }
            for group in members.does.iter() {
                for member in group.members.iter() {
                    let _ = writeln!(
                        out,
                        "    does ({}): {}{}",
                        group.receiver.name(),
                        member.decl.name,
                        member
                            .summary
                            .as_ref()
                            .map(|summary| format!(" — {summary}"))
                            .unwrap_or_default()
                    );
                }
            }
            for member in members.other.iter() {
                let _ = writeln!(out, "    other: {} {}", member.decl.kind_name(), member.decl.name);
            }
        }
        Known::Unknown(missing) => {
            let _ = writeln!(out, "  members: {}", gap(missing));
        }
    }
    let _ = writeln!(out, "  rose:");
    relations(&mut out, "  up (is)", &page.rose.up);
    relations(&mut out, "  down (made of)", &page.rose.down);
    relations(&mut out, "  left (from)", &page.rose.left);
    relations(&mut out, "  right (to)", &page.rose.right);
    relations(&mut out, "  implemented by", &page.rose.implemented_by);
    match &page.references {
        Known::Known(sites) => {
            let _ = writeln!(out, "  references ({}):", sites.len());
            for site in sites.iter() {
                let _ = writeln!(
                    out,
                    "    - {} {} · {} · {} · {}",
                    site.site.kind_name(),
                    site.site.name,
                    link_name(site.relation),
                    confidence_name(site.confidence),
                    known(&site.span, |span| format!(
                        "{}@{}..{}",
                        span.file, span.bytes.start, span.bytes.end
                    ))
                );
            }
        }
        Known::Unknown(missing) => {
            let _ = writeln!(out, "  references: {}", gap(missing));
        }
    }
    let _ = writeln!(
        out,
        "  outline: {}",
        known(&page.outline, |position| {
            let ancestors = position
                .ancestors
                .iter()
                .map(|decl| decl.name.to_string())
                .collect::<Vec<_>>()
                .join(" › ");
            format!(
                "{ancestors} › [{}] ({} siblings, index {:?})",
                page.identity.name,
                position.siblings.len(),
                position.index
            )
        })
    );
    out
}

/// Renders one package dossier.
#[must_use]
#[allow(clippy::too_many_lines)] // a flat field-by-field dump
pub fn package_text(dossier: &PackageDossier) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "package {}", dossier.package);
    match &dossier.record {
        Known::Known(record) => {
            let _ = writeln!(out, "  name: {} ({:?})", record.name, record.source);
            let _ = writeln!(out, "  version: {}", known(&record.version, ToString::to_string));
            let _ = writeln!(out, "  ecosystem: {}", known(&record.ecosystem, ToString::to_string));
            let _ = writeln!(out, "  standing: {}", known(&record.standing, |standing| standing.name().to_owned()));
            let _ = writeln!(out, "  downloads: {}", known(&record.downloads, |downloads| format!("{downloads:?}")));
            let _ = writeln!(out, "  bytes: {}", known(&record.bytes, ToString::to_string));
            let _ = writeln!(out, "  advisory: {}", known(&record.advisory, |advisory| format!(
                "{} advisories, worst {:?}, decision {}, coverage {:?}",
                advisory.advisories, advisory.worst, advisory.decision, advisory.coverage
            )));
            let _ = writeln!(out, "  description: {}", known(&record.description, ToString::to_string));
            let _ = writeln!(out, "  license: {}", known(&record.license, ToString::to_string));
        }
        Known::Unknown(missing) => {
            let _ = writeln!(out, "  record: {}", gap(missing));
        }
    }
    let _ = writeln!(out, "  versions: {}", known(&dossier.versions, |versions| {
        versions
            .iter()
            .map(|entry| format!(
                "{}{} ({})",
                entry.version,
                if entry.current { "*" } else { "" },
                entry.standing.name()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    }));
    let _ = writeln!(out, "  dependencies: {}", known(&dossier.dependencies, |dependencies| {
        dependencies
            .iter()
            .map(|dependency| format!("{} {} [{}]", dependency.name, dependency.requirement, dependency.scope.name()))
            .collect::<Vec<_>>()
            .join(", ")
    }));
    let _ = writeln!(out, "  dependents: {}", known(&dossier.dependents, |dependents| {
        format!("{} packages", dependents.len())
    }));
    let _ = writeln!(out, "  readme: {}", known(&dossier.readme, |blocks| format!("{} blocks", blocks.len())));
    match &dossier.outline {
        Known::Known(tree) => {
            let _ = writeln!(
                out,
                "  outline: {} declarations{}",
                tree.count(),
                if tree.complete { "" } else { " (incomplete)" }
            );
            for root in tree.roots.iter() {
                let items = root
                    .children
                    .iter()
                    .take(12)
                    .map(|node| format!("{} {}", node.decl.kind_name(), node.decl.name))
                    .collect::<Vec<_>>();
                let _ = writeln!(
                    out,
                    "    {} {} ({}): {}{}",
                    root.decl.kind_name(),
                    root.decl.name,
                    root.children.len(),
                    items.join(", "),
                    if root.children.len() > 12 { ", …" } else { "" }
                );
            }
        }
        Known::Unknown(missing) => {
            let _ = writeln!(out, "  outline: {}", gap(missing));
        }
    }
    out
}

/// Renders one source view (text truncated to 40 lines).
#[must_use]
pub fn source_text(view: &SourceView) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "source {} {}", view.symbol.kind_name(), view.symbol.name);
    let _ = writeln!(out, "  file: {}", known(&view.file, ToString::to_string));
    let _ = writeln!(out, "  declaration: {}", known(&view.declaration, |span| format!("lines {}..={}", span.first, span.last)));
    let _ = writeln!(out, "  identifiers: {}", known(&view.identifiers, |spans| {
        let text = view.text.known();
        spans
            .iter()
            .take(24)
            .map(|span| {
                let word = text
                    .and_then(|text| text.text.get(span.span.range()))
                    .unwrap_or("?");
                format!("{word}@{}→{}", span.span.start, span.link.target)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }));
    let _ = writeln!(out, "  uses here: {}", known(&view.uses, |spans| format!("{} spans", spans.len())));
    let _ = writeln!(out, "  uses elsewhere: {}", view.uses_elsewhere.len());
    match &view.text {
        Known::Known(text) => {
            let _ = writeln!(out, "  text ({:?}, from line {}):", text.origin, text.first_line);
            for (offset, line) in text.text.lines().enumerate().take(40) {
                let _ = writeln!(out, "    {:>5} │ {line}", text.first_line as usize + offset);
            }
        }
        Known::Unknown(missing) => {
            let _ = writeln!(out, "  text: {}", gap(missing));
        }
    }
    out
}

/// Renders one search page.
#[must_use]
pub fn search_text(page: &SearchPage) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "search {:?}: {} rows{} · lanes {}",
        page.query,
        page.rows.len(),
        if page.next.is_some() { " (more)" } else { "" },
        page.coverage
            .lanes()
            .iter()
            .map(|lane| format!("{} {}", lane.name(), lane.mark(None)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for row in page.rows.iter().take(25) {
        let _ = writeln!(
            out,
            "  {:>3}. {} {} [{}] score {} — {}",
            row.rank,
            row.decl.kind_name(),
            row.decl.name,
            row.reason.name(),
            known(&row.score, ToString::to_string),
            row.snippet.as_deref().unwrap_or("")
        );
    }
    out
}

/// Renders the Orbit model.
#[must_use]
pub fn orbit_text(model: &OrbitModel) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "orbit");
    let _ = writeln!(out, "  indexed: {}", known(&model.indexed, |packages| {
        packages
            .iter()
            .map(|package| format!("{} ({:?})", package.name, package.readiness))
            .collect::<Vec<_>>()
            .join(", ")
    }));
    let _ = writeln!(out, "  projects: {}", known(&model.projects, |projects| format!("{} projects", projects.len())));
    let _ = writeln!(out, "  explore: {}", known(&model.explore, |records| format!("{} registry records", records.len())));
    let _ = writeln!(out, "  tree: {}", known(&model.tree, |nodes| format!("{} nodes", nodes.len())));
    out
}

/// Renders the health model.
#[must_use]
pub fn health_text(model: &HealthModel) -> String {
    let mut out = String::new();
    let lanes = model
        .lanes
        .lanes()
        .iter()
        .map(|lane| format!("{} {}", lane.name(), lane.mark(None)))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "health: {} rows · lanes {lanes}", model.rows);
    let ingest = &model.ingest;
    let _ = writeln!(
        out,
        "  ingest: {}/{} files indexed, {} unavailable, {} declarations, gem {}/12",
        ingest.files_indexed,
        ingest.files_discovered,
        ingest.files_unavailable,
        ingest.declarations,
        ingest.facets().map_or_else(|| "-".to_owned(), |facets| facets.to_string())
    );
    let _ = writeln!(
        out,
        "  capabilities: {} ready, {} not ready",
        model.ready_capabilities.len(),
        model.missing_capabilities.len()
    );
    out
}

/// Renders every resident page resource in the store, focused keys first.
#[must_use]
pub fn render_text(store: &DataStore) -> String {
    let mut out = String::new();
    let stats = store.stats();
    let (queued, running) = store.pool_load();
    let _ = writeln!(
        out,
        "data plane · root {} · focused {} · pool {queued} queued / {running} running · {} landed, {} superseded, {} cancelled",
        store.snapshot().key(),
        store.focused().len(),
        stats.landed,
        stats.superseded,
        stats.cancelled
    );
    for key in store.pages().keys() {
        let _ = writeln!(out, "\n── {key}");
        let text = match &key {
            PageKey::Symbol(symbol) => section(&store.symbol(symbol), symbol_text),
            PageKey::Source(symbol) => section(&store.source(symbol), source_text),
            PageKey::Package(package) => section(&store.package(package), package_text),
            PageKey::Search(query) => section(&store.search(query), search_text),
            PageKey::Orbit => section(&store.orbit(), orbit_text),
            PageKey::Health => section(&store.health(), health_text),
        };
        out.push_str(&text);
    }
    out
}

fn section<T>(resource: &Resource<T>, show: impl FnOnce(&T) -> String) -> String {
    let mut out = status(resource);
    out.push('\n');
    if let Some(value) = resource.loaded_value() {
        out.push_str(&show(value));
    }
    out
}

/// A plain-text window onto the store. Re-renders only on store events.
pub struct DebugPageView {
    store: Entity<DataStore>,
    text: SharedString,
    _events: Subscription,
}

impl DebugPageView {
    /// Creates the view and ensures `keys` through the store.
    pub fn new(store: Entity<DataStore>, keys: Vec<PageKey>, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe(&store, |view: &mut Self, store, _event: &StoreEvent, cx| {
            let text = SharedString::from(render_text(store.read(cx)));
            if text != view.text {
                view.text = text;
                cx.notify();
            }
        });
        store.update(cx, |store, cx| {
            for key in keys {
                store.ensure(key, cx);
            }
        });
        let text = SharedString::from(render_text(store.read(cx)));
        Self {
            store,
            text,
            _events: events,
        }
    }

    /// Returns the store the view reads.
    #[must_use]
    pub const fn store(&self) -> &Entity<DataStore> {
        &self.store
    }
}

impl Render for DebugPageView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("debug-page")
            .size_full()
            .overflow_y_scroll()
            .p(px(12.0))
            .bg(gpui::rgb(0x0f_12_18))
            .text_color(gpui::rgb(0xd8_de_e9))
            .font_family(facet::fonts::family(facet::tokens::ty::CODE))
            .text_size(px(12.0))
            .child(self.text.clone())
    }
}

/// Parses `NUDOX_DEBUG_PAGE` keys: `;`-separated `symbol:…`, `source:…`,
/// `package:…`, `search:…`, `orbit`, `health`.
#[must_use]
pub fn parse_keys(spec: &str) -> Vec<PageKey> {
    use crate::model::pages::{PackageRef, SearchQuery, SymbolRef};
    spec.split(';')
        .filter_map(|part| {
            let part = part.trim();
            let (kind, value) = part.split_once(':').unwrap_or((part, ""));
            match kind {
                "symbol" => SymbolRef::new(value).ok().map(PageKey::Symbol),
                "source" => SymbolRef::new(value).ok().map(PageKey::Source),
                "package" => PackageRef::parse(value).ok().map(PageKey::Package),
                "search" => SearchQuery::new(value, SearchQuery::DEFAULT_LIMIT)
                    .ok()
                    .map(PageKey::Search),
                "orbit" => Some(PageKey::Orbit),
                "health" => Some(PageKey::Health),
                _ => None,
            }
        })
        .collect()
}

/// Opens the debug page as its own window.
pub fn open_window(cx: &mut App, store: Entity<DataStore>, keys: Vec<PageKey>) {
    let options = gpui::WindowOptions {
        titlebar: Some(gpui::TitlebarOptions {
            title: Some("Nudox data plane (debug)".into()),
            ..gpui::TitlebarOptions::default()
        }),
        ..gpui::WindowOptions::default()
    };
    if let Err(error) = cx.open_window(options, move |_, cx| {
        cx.new(|cx| DebugPageView::new(store, keys, cx))
    }) {
        eprintln!("backend-desktop: open debug page: {error}");
    }
}
