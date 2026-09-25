//! The shelf region: the book you are in and its outline, or your library on
//! Orbit, or the settings pages — and the 42 px kspine it collapses to.
//!
//! Rows at rest are a kind mark and a name (§6.2). The region lays itself
//! out from its measured width: wide enough for the shelf, it draws the
//! shelf at its resting width (so an animating column clips it instead of
//! reflowing it); narrow, it draws the spine; in between — only while the
//! shell animates the column — both, crossfaded.

use super::focus::{Act, Target, Targets};
use super::kit::{HoverIntent, keycap, kind_of, package_route, symbol_route, text};
use super::region::{Links, Region, RegionCore};
use super::thread::{route_package, route_symbol, settings_name};
use crate::model::AppSnapshot;
use crate::model::pages::{OutlineNode, PackageRef, PageKey, SymbolRef};
use crate::navigation::{Intent, Overlay, Route, SettingsPage};
use crate::runtime::store::{Branch, DataStore};
use facet::icons::{self, Icon, IconSize, Kind, KindSize};
use facet::motion::Motion;
use facet::paint::{Bevel, Chamfer, cut};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Palette, Space};
use gpui::{
    AnyElement, ClickEvent, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, ScrollStrategy, SharedString, StatefulInteractiveElement, Styled,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;

/// One shelf row, flattened.
#[derive(Clone)]
struct Row {
    id: SharedString,
    depth: u8,
    mark: RowMark,
    name: SharedString,
    current: bool,
    /// A folded group: activating it toggles instead of navigating.
    group: Option<(SymbolRef, bool)>,
    /// What a click does.
    act: Option<Act>,
    /// Hover-intent prefetch.
    warm: Option<PageKey>,
    /// S peels to source.
    source: Option<SymbolRef>,
}

#[derive(Clone, Copy)]
enum RowMark {
    Kind(Kind),
    Icon(Icon),
}

/// The header above the rows.
#[derive(Clone, Default)]
struct Head {
    crumb: Option<(SharedString, Route)>,
    book: Option<(SharedString, SharedString)>,
}

/// The shelf region.
pub(crate) struct Shelf {
    core: RegionCore,
    links: Links,
    pub(crate) targets: Targets,
    motion: Motion,
    hover: HoverIntent,
    /// Groups the user opened or closed by hand.
    toggled: BTreeSet<SymbolRef>,
    /// The resting width of the full shelf, from the shell.
    rest: Pixels,
    /// The resting width of the spine, from the shell.
    spine: Pixels,
    rows: Rc<Vec<Row>>,
    scroll: UniformListScrollHandle,
}

impl Shelf {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        Self {
            core: RegionCore::new(store, &[Branch::Route, Branch::Overlay, Branch::Workspace]),
            links,
            targets: Targets::default(),
            motion: Motion::new(),
            hover: HoverIntent::default(),
            toggled: BTreeSet::new(),
            rest: px(264.0),
            spine: px(42.0),
            rows: Rc::new(Vec::new()),
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }

    /// The shell tells the shelf its resting widths (no notify: a change of
    /// resting width is always a change of the column's bounds too).
    pub(crate) fn set_rest(&mut self, rest: Pixels, spine: Pixels) {
        self.rest = rest;
        self.spine = spine;
    }

    /// Keeps the focused row on screen after a keyboard walk.
    pub(crate) fn reveal_focused(&self) {
        if let Some(index) = self
            .targets
            .focused()
            .and_then(|id| self.rows.iter().position(|row| &row.id == id))
        {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    fn toggle(&mut self, group: SymbolRef, cx: &mut Context<Self>) {
        if !self.toggled.remove(&group) {
            self.toggled.insert(group);
        }
        cx.notify();
    }
}

impl Region for Shelf {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }

    fn keys(&self, snapshot: &AppSnapshot) -> Vec<PageKey> {
        if matches!(snapshot.overlay(), Some(Overlay::Settings(_))) {
            return Vec::new();
        }
        let mut keys = vec![PageKey::Orbit];
        if let Some(package) = route_package(snapshot.route()) {
            keys.push(PageKey::Package(package));
        }
        keys
    }

    fn observe(&mut self, event: &crate::runtime::store::StoreEvent, _: &DataStore) {
        // A new book starts with only the current group open.
        if event.is_branch(Branch::Route) {
            self.toggled.clear();
        }
    }
}

impl Render for Shelf {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        self.targets.begin();
        let measure = self.core.measure(cx);
        let facet = cx.facet();
        let palette = facet.palette();
        let width = self.core.width();
        let snapshot = self.links.snapshot(cx);
        let (head, rows) = self.build(&snapshot, cx);
        for row in &rows {
            if let Some(act) = &row.act {
                self.targets.push(Target {
                    id: row.id.clone(),
                    label: row.name.clone(),
                    act: Rc::clone(act),
                    peek: row.warm.clone(),
                    source: row.source.clone(),
                });
            }
        }
        self.rows = Rc::new(rows);

        // Where the column sits between spine and shelf: 0 = spine, 1 = shelf.
        let span = (self.rest - self.spine).max(px(1.0));
        let open = ((width - self.spine) / span).clamp(0.0, 1.0);
        let open = open * open * (3.0 - 2.0 * open);
        let rest_measure = Measure::new(self.rest, &facet);
        let mut root = div()
            .id("shelf")
            .relative()
            .size_full()
            .overflow_hidden()
            .border_r_1()
            .border_color(palette.line1.hsla());
        if open > 0.001 {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(self.rest.max(width))
                    .opacity(open)
                    .child(self.shelf(&head, &rest_measure, palette, facet.reveal.keys, cx)),
            );
        }
        if open < 0.999 {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(self.spine)
                    .opacity(1.0 - open)
                    .child(self.spine_column(&measure, palette)),
            );
        }
        let _ = window;
        root.child(self.targets.glow(&self.motion, &measure))
    }
}

impl Shelf {
    fn build(&self, snapshot: &AppSnapshot, cx: &mut Context<Self>) -> (Head, Vec<Row>) {
        if let Some(Overlay::Settings(current)) = snapshot.overlay() {
            return (
                Head {
                    crumb: Some(("Nudox".into(), snapshot.route().clone())),
                    book: None,
                },
                self.settings_rows(current),
            );
        }
        match snapshot.route() {
            Route::Orbit(_) => self.orbit_rows(snapshot, cx),
            route => self.book_rows(route, snapshot, cx),
        }
    }

    fn settings_rows(&self, current: SettingsPage) -> Vec<Row> {
        [
            (SettingsPage::Appearance, Icon::Eye),
            (SettingsPage::Index, Icon::Server),
            (SettingsPage::Help, Icon::Key),
            (SettingsPage::Diagnostics, Icon::Info),
        ]
        .into_iter()
        .map(|(page, icon)| {
            let links = self.links.clone();
            Row {
                id: format!("settings-{}", page.as_str()).into(),
                depth: 0,
                mark: RowMark::Icon(icon),
                name: settings_name(page).into(),
                current: page == current,
                group: None,
                act: Some(Rc::new(move |_, cx| links.dispatch(Intent::OpenSettings(page), cx))),
                warm: None,
                source: None,
            }
        })
        .collect()
    }

    fn orbit_rows(&self, snapshot: &AppSnapshot, cx: &mut Context<Self>) -> (Head, Vec<Row>) {
        let store = self.links.store.read(cx);
        let orbit = store.orbit();
        let workspace = snapshot.workspace();
        let mut rows = Vec::new();
        for project in workspace.projects.iter() {
            let links = self.links.clone();
            let id = project.id.clone();
            rows.push(Row {
                id: format!("project-{}", project.path).into(),
                depth: 0,
                mark: RowMark::Kind(Kind::Module),
                name: project.label.to_string().into(),
                current: workspace.active.as_ref() == Some(&project.id),
                group: None,
                act: Some(Rc::new(move |_, cx| links.dispatch(Intent::ActivateProject(id.clone()), cx))),
                warm: None,
                source: None,
            });
        }
        let indexed = orbit.loaded_value().and_then(|model| model.indexed.known().cloned());
        let count = indexed.as_ref().map_or(0, |indexed| indexed.len());
        for package in indexed.iter().flat_map(|indexed| indexed.iter()) {
            let links = self.links.clone();
            let route = package_route(&package.package);
            rows.push(Row {
                id: format!("package-{}", package.package).into(),
                depth: 0,
                mark: RowMark::Kind(Kind::Package),
                name: package.name.to_string().into(),
                current: false,
                group: None,
                act: route.map(|route| -> Act { Rc::new(move |_, cx| links.dispatch(Intent::Navigate(route.clone()), cx)) }),
                warm: Some(PageKey::Package(package.package.clone())),
                source: None,
            });
        }
        let book = (
            SharedString::from("Library"),
            SharedString::from(format!(
                "{} project{} · {count} package{}",
                workspace.projects.len(),
                if workspace.projects.len() == 1 { "" } else { "s" },
                if count == 1 { "" } else { "s" }
            )),
        );
        (Head { crumb: None, book: Some(book) }, rows)
    }

    fn book_rows(&self, route: &Route, snapshot: &AppSnapshot, cx: &mut Context<Self>) -> (Head, Vec<Row>) {
        let Some(package) = route_package(route) else {
            return (Head::default(), Vec::new());
        };
        let current = route_symbol(route);
        let store = self.links.store.read(cx);
        let dossier = store.package(&package);
        let dossier = dossier.loaded_value();
        let crumb = match route {
            _ => Some((
                snapshot
                    .workspace()
                    .projects
                    .iter()
                    .find(|project| snapshot.workspace().active.as_ref() == Some(&project.id))
                    .map_or_else(|| SharedString::from("Library"), |project| project.label.to_string().into()),
                Route::Orbit(crate::navigation::OrbitRoute::Home),
            )),
        };
        let version = dossier
            .and_then(|dossier| dossier.record.known())
            .and_then(|record| record.version.known().map(ToString::to_string))
            .unwrap_or_default();
        let head = Head {
            crumb,
            book: Some((package.display_name().to_owned().into(), version.into())),
        };
        let Some(tree) = dossier.and_then(|dossier| dossier.outline.known()) else {
            return (head, Vec::new());
        };
        let mut rows = Vec::new();
        let weak = cx.weak_entity();
        for root in tree.roots.iter() {
            let holds_current = current
                .as_ref()
                .is_some_and(|current| contains(root, current));
            let open = holds_current != self.toggled.contains(&root.decl.coordinate);
            self.push_node(&weak, &mut rows, root, 0, &package, current.as_ref(), open);
            if open {
                for child in root.children.iter() {
                    self.push_node(&weak, &mut rows, child, 1, &package, current.as_ref(), false);
                }
            }
        }
        (head, rows)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_node(
        &self,
        weak: &gpui::WeakEntity<Self>,
        rows: &mut Vec<Row>,
        node: &OutlineNode,
        depth: u8,
        package: &PackageRef,
        current: Option<&SymbolRef>,
        open: bool,
    ) {
        let symbol = node.decl.coordinate.clone();
        let is_group = depth == 0 && !node.children.is_empty();
        let links = self.links.clone();
        let act: Act = if is_group {
            let group = symbol.clone();
            let weak = weak.clone();
            Rc::new(move |_, cx| {
                let _ = weak.update(cx, |shelf, cx| shelf.toggle(group.clone(), cx));
            })
        } else {
            match symbol_route(package.as_str(), &symbol) {
                Some(route) => Rc::new(move |_, cx| links.dispatch(Intent::Navigate(route.clone()), cx)),
                None => return,
            }
        };
        rows.push(Row {
            id: symbol.as_str().to_owned().into(),
            depth,
            mark: RowMark::Kind(kind_of(node.decl.kind)),
            name: shelf_name(node).into(),
            current: current == Some(&symbol),
            group: is_group.then(|| (symbol.clone(), open)),
            act: Some(act),
            warm: (!is_group).then(|| PageKey::Symbol(symbol.clone())),
            source: (!is_group).then_some(symbol),
        });
    }

    fn shelf(&mut self, head: &Head, measure: &Measure, palette: &Palette, keys: bool, cx: &mut Context<Self>) -> AnyElement {
        let gutter = measure.space(Space::Roomy);
        let mut column = div().size_full().flex().flex_col().pt(measure.space(Space::Roomy));
        if let Some((crumb, route)) = &head.crumb {
            let links = self.links.clone();
            let route = route.clone();
            column = column.child(
                div()
                    .id("shelf-crumb")
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Snug))
                    .px(gutter)
                    .pb(measure.space(Space::Base))
                    .child(icons::chevron(IconSize::S12, palette.ink3).size(measure.icon(12.0)).with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI))))
                    .child(text(ty::SMALL, measure, palette.ink2).child(crumb.clone()))
                    .on_click(move |_: &ClickEvent, _, cx| links.dispatch(Intent::Navigate(route.clone()), cx)),
            );
        }
        if let Some((name, detail)) = &head.book {
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Roomy))
                    .px(gutter)
                    .pb(measure.space(Space::Roomy))
                    .child(super::kit::kind_mark(Kind::Package, KindSize::Lg, &measure, palette))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .child(text(ty::HEAD, measure, palette.ink0).child(name.clone()))
                            .child(text(ty::MONO_SMALL, measure, palette.ink3).child(detail.clone())),
                    ),
            );
        }
        let links = self.links.clone();
        column = column.child(
            div().px(gutter).pb(measure.space(Space::Base)).child(
                div()
                    .id("shelf-filter")
                    .relative()
                    .child(
                        cut()
                            .chamfer(Chamfer::Sm)
                            .bevel(Bevel::Rest)
                            .fill(palette.well)
                            .h(measure.control(facet::Control::Medium))
                            .px(measure.space(Space::Base))
                            .flex()
                            .items_center()
                            .gap(measure.space(Space::Snug))
                            .child(icons::ui(Icon::Filter, IconSize::S12, palette.ink4).size(measure.icon(12.0)))
                            .child(text(ty::SMALL, measure, palette.ink4).child("Filter")),
                    )
                    .on_click(move |_: &ClickEvent, _, cx| links.shell(cx, |shell, cx| shell.open_ask(false, cx)))
                    .children(keycap(keys, "⌘K", measure)),
            ),
        );
        let count = self.rows.len();
        let row_measure = *measure;
        column
            .child(
                uniform_list(
                    "shelf-rows",
                    count,
                    cx.processor(move |shelf: &mut Self, range: Range<usize>, _window, cx| {
                        shelf.render_rows(range, &row_measure, cx)
                    }),
                )
                .flex_1()
                .track_scroll(&self.scroll),
            )
            .into_any_element()
    }

    fn render_rows(&mut self, range: Range<usize>, measure: &Measure, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let palette = cx.facet().palette();
        let rows = Rc::clone(&self.rows);
        range
            .filter_map(|index| rows.get(index).cloned())
            .map(|row| self.row(row, measure, palette, cx))
            .collect()
    }

    fn row(&mut self, row: Row, measure: &Measure, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let height = measure.row() + measure.space(Space::Tight);
        let indent = measure.space(Space::Roomy) + measure.space(Space::Gutter) * f32::from(row.depth);
        let ink: Hsla = if row.current { palette.ink0.into() } else { palette.ink1.into() };
        let mark = match row.mark {
            RowMark::Kind(kind) => super::kit::kind_mark(kind, KindSize::Sm, &measure, palette),
            RowMark::Icon(icon) => icons::ui(icon, IconSize::S14, palette.ink2)
                .size(measure.icon(14.0))
                .into_any_element(),
        };
        let focused = self.targets.is_focused(&row.id);
        let mut element = div()
            .id(row.id.clone())
            .relative()
            .h(height)
            .w_full()
            .flex()
            .items_center()
            .gap(measure.space(Space::Base))
            .pl(indent)
            .pr(measure.space(Space::Roomy))
            .hover(|style| style.bg(palette.tint))
            .child(mark)
            .child(
                text(ty::MONO_ROW, measure, ink)
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(row.name.clone()),
            );
        if row.current {
            element = element
                .bg(palette.tint)
                .child(div().absolute().left_0().top_0().bottom_0().w(px(2.0)).bg(palette.mint.base));
        }
        let _ = focused;
        if let Some((group, _open)) = row.group.clone() {
            element = element.on_click(cx.listener(move |shelf, _: &ClickEvent, _, cx| {
                shelf.targets.focus(row_id(&group));
                shelf.toggle(group.clone(), cx);
            }));
        } else if let Some(act) = row.act.clone() {
            let id = row.id.clone();
            element = element.on_click(cx.listener(move |shelf, _: &ClickEvent, window, cx| {
                shelf.targets.focus(id.clone());
                act(window, cx);
            }));
        }
        if let Some(key) = row.warm.clone() {
            element = element.on_hover(cx.listener(move |shelf, hovered: &bool, _, cx| {
                let links = shelf.links.clone();
                shelf.hover.hover(key.clone(), *hovered, &links, cx);
            }));
        }
        self.targets.track(row.id, element).into_any_element()
    }

    fn spine_column(&mut self, measure: &Measure, palette: &Palette) -> AnyElement {
        let side = px(28.0 * measure.scale());
        let gap = measure.space(Space::Snug);
        let mut column = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(gap)
            .pt(measure.space(Space::Roomy))
            .overflow_hidden();
        for row in self.rows.iter().filter(|row| row.depth == 0 || row.current).take(24) {
            let mark = match row.mark {
                RowMark::Kind(kind) => super::kit::kind_mark(kind, KindSize::Sm, &measure, palette),
                RowMark::Icon(icon) => icons::ui(icon, IconSize::S14, palette.ink2).into_any_element(),
            };
            let act = row.act.clone();
            let mut cell = div()
                .id(SharedString::from(format!("spine-{}", row.id)))
                .relative()
                .flex_none()
                .size(side)
                .flex()
                .items_center()
                .justify_center()
                .opacity(if row.current { 1.0 } else { 0.62 })
                .hover(|style| style.opacity(1.0))
                .child(mark);
            if row.current {
                cell = cell.child(div().absolute().left(-measure.space(Space::Snug)).top_0().bottom_0().w(px(2.0)).bg(palette.mint.base));
            }
            if let Some(act) = act {
                cell = cell.on_click(move |_: &ClickEvent, window, cx| act(window, cx));
            }
            column = column.child(cell);
        }
        column.into_any_element()
    }
}

/// A module row reads as its module (`glyph`), not its file (`glyph.rs`).
fn shelf_name(node: &OutlineNode) -> String {
    let name = node.decl.name.as_ref();
    if node.decl.kind == Some(backend_library::DeclarationKind::Module)
        && let Some((stem, extension)) = name.rsplit_once('.')
        && matches!(extension, "rs" | "ts" | "tsx" | "js" | "py" | "go" | "java" | "cs" | "c" | "cc" | "cpp" | "h" | "hpp")
        && !stem.is_empty()
    {
        return stem.to_owned();
    }
    name.to_owned()
}

fn row_id(symbol: &SymbolRef) -> SharedString {
    symbol.as_str().to_owned().into()
}

/// Whether `node`'s subtree holds `symbol`.
fn contains(node: &OutlineNode, symbol: &SymbolRef) -> bool {
    &node.decl.coordinate == symbol || node.children.iter().any(|child| contains(child, symbol))
}
