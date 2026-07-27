//! The Refs and Impls tables (§16).
//!
//! # Shape
//!
//! References are the one surface on a symbol page that can be genuinely huge —
//! a popular trait has tens of thousands of them. So this is `uniform_list` from
//! the first row (LD-6), over a *flattened* index: one `Vec<Row>` where an entry
//! is either a file header or a reference under it. Collapsing a file group
//! rebuilds that flat vector once, at update time; the list itself never learns
//! about grouping.
//!
//! Columns are the Sourcegraph pattern: path · line · kind · precision. The
//! precision badge is the honest part — a textual match and a resolved
//! cross-reference are not the same claim, and conflating them is how reference
//! search loses trust.
//!
//! # Grouping without sorting
//!
//! `render` may not sort or filter (§1.1.4), and neither does the projection:
//! [`RefsTable::append_page`] is a single linear pass that starts a new group
//! whenever the file changes from the previous row. The engine emits a page's
//! rows file-contiguous; if it ever does not, the same file simply appears as
//! two groups — degraded, never wrong, never a panic.
//!
//! # Motion
//!
//! Pages append with `row.cascade` (§5.2), gated on the arrival window so
//! scrolling back over already-landed rows never replays their entrance
//! (§4.1 entrance identity rule).

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, UniformListScrollHandle, Window, div,
    prelude::FluentBuilder as _, uniform_list,
};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _};
use nudox_engine::wire::{ImplRow, ImplsPage, RefRow, RefsPage, SymbolKey};

use crate::motion::declarative::{entrance_id, row_enter};
use crate::motion::tokens::{MotionTokens, ROW_CASCADE_WINDOW};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::ui::{Badge, EmptyState};

use super::header::shared;

/// How confident the engine is that a reference really is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    /// Resolved through the corpus — this *is* a reference.
    Precise,
    /// A textual or heuristic match — probably a reference.
    Textual,
    /// A claim we do not recognise; shown verbatim (LD-7).
    Other,
}

impl Precision {
    /// Classify the engine's free-form kind tag. Projection time only.
    fn classify(tag: &str) -> Self {
        // Lowercase comparison without allocating: the tags are ASCII.
        let precise = tag.eq_ignore_ascii_case("precise")
            || tag.eq_ignore_ascii_case("resolved")
            || tag.eq_ignore_ascii_case("definition");
        if precise {
            return Self::Precise;
        }
        let textual = tag.eq_ignore_ascii_case("textual")
            || tag.eq_ignore_ascii_case("search")
            || tag.eq_ignore_ascii_case("fuzzy")
            || tag.eq_ignore_ascii_case("heuristic");
        if textual {
            Self::Textual
        } else {
            Self::Other
        }
    }
}

/// One reference row, fully render-ready.
#[derive(Clone, Debug)]
struct RefRowView {
    target: SymbolKey,
    /// Path with the trailing `:line` removed, if there was one.
    path: SharedString,
    /// The line number, when the engine encoded one into `path`.
    line: Option<SharedString>,
    /// The engine's raw kind tag — always shown, whatever it says.
    kind: SharedString,
    precision: Precision,
}

/// Split `src/foo.rs:120` into `("src/foo.rs", Some("120"))`.
///
/// TODO(wire): `RefRow` has no `line` field, so the line number can only reach
/// us encoded in `path`. When the wire type grows `line: u32` this function
/// disappears and the column becomes first-class.
fn split_line(path: &str) -> (&str, Option<&str>) {
    match path.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
            (head, Some(tail))
        }
        _ => (path, None),
    }
}

/// The file grouping of a run of references.
#[derive(Clone, Debug)]
struct Group {
    file: SharedString,
    /// Pre-formatted member count, e.g. `"12"`.
    count: SharedString,
    collapsed: bool,
    rows: Vec<RefRowView>,
}

/// One entry in the flattened list the `uniform_list` actually renders.
#[derive(Clone, Copy, Debug)]
enum Flat {
    Header(usize),
    Row { group: usize, row: usize },
}

// ─────────────────────────────────────────────────────────────────────────────
// RefsTable
// ─────────────────────────────────────────────────────────────────────────────

/// The grouped, virtualized references table.
pub struct RefsTable {
    groups: Vec<Group>,
    flat: Vec<Flat>,
    consumed_pages: usize,
    total: u64,
    /// Pre-formatted total, for the tab's count label.
    total_text: SharedString,
    scroll: UniformListScrollHandle,
    /// Generation stamp: part of every entrance id (LD-19).
    generation: u64,
    /// When the last page landed — the `row.cascade` window opens from here.
    last_page: Option<Instant>,
}

impl Default for RefsTable {
    fn default() -> Self {
        Self::new()
    }
}

impl RefsTable {
    /// An empty table.
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            flat: Vec::new(),
            consumed_pages: 0,
            total: 0,
            total_text: SharedString::from("0"),
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            last_page: None,
        }
    }

    /// Drop everything for a new generation.
    pub fn reset(&mut self, generation: u64) {
        self.groups.clear();
        self.flat.clear();
        self.consumed_pages = 0;
        self.total = 0;
        self.total_text = SharedString::from("0");
        self.generation = generation;
        self.last_page = None;
    }

    /// Total references reported by the engine.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Pre-formatted total for the tab label.
    pub fn total_text(&self) -> SharedString {
        self.total_text.clone()
    }

    /// `true` when no page has landed yet.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Consume any pages the store has that we have not projected yet.
    ///
    /// Returns `true` if anything changed.
    pub fn sync(&mut self, pages: &[RefsPage]) -> bool {
        if self.consumed_pages >= pages.len() {
            return false;
        }
        for page in &pages[self.consumed_pages..] {
            self.append_page(page);
        }
        self.consumed_pages = pages.len();
        self.last_page = Some(Instant::now());
        self.rebuild_flat();
        true
    }

    /// One linear pass: no sort, no filter, no second look at earlier rows.
    fn append_page(&mut self, page: &RefsPage) {
        if page.total != self.total {
            self.total = page.total;
            self.total_text = SharedString::from(page.total.to_string());
        }
        for row in page.refs.iter() {
            let view = project_ref(row);
            let start_new = match self.groups.last() {
                Some(g) => g.file != view.path,
                None => true,
            };
            if start_new {
                self.groups.push(Group {
                    file: view.path.clone(),
                    count: SharedString::from("0"),
                    collapsed: false,
                    rows: Vec::new(),
                });
            }
            if let Some(group) = self.groups.last_mut() {
                group.rows.push(view);
                group.count = SharedString::from(group.rows.len().to_string());
            }
        }
    }

    /// Toggle a file group. Returns `true` if the flat index changed.
    pub fn toggle_group(&mut self, group_ix: usize) -> bool {
        match self.groups.get_mut(group_ix) {
            Some(group) => {
                group.collapsed = !group.collapsed;
                self.rebuild_flat();
                true
            }
            None => false,
        }
    }

    fn rebuild_flat(&mut self) {
        self.flat.clear();
        for (gx, group) in self.groups.iter().enumerate() {
            self.flat.push(Flat::Header(gx));
            if !group.collapsed {
                self.flat
                    .extend((0..group.rows.len()).map(|rx| Flat::Row { group: gx, row: rx }));
            }
        }
    }

    /// Render the table, or a designed empty state (LD-16).
    ///
    /// `streaming` suppresses the empty state while pages are still arriving —
    /// "no references" is a *finding*, and we do not report it prematurely.
    pub fn render(
        &self,
        streaming: bool,
        on_open: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static,
        on_toggle: impl Fn(usize, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;
        let scale = ext.motion_scale;

        if self.flat.is_empty() {
            return if streaming {
                // Nothing to say yet; the tab's `⟳` already says it.
                div().w_full().into_any_element()
            } else {
                EmptyState::new(
                    IconName::Search,
                    SharedString::from("No references"),
                    SharedString::from("Nothing in the loaded corpus mentions this symbol."),
                    MotionTokens::new(scale),
                )
                .into_any_element()
            };
        }

        let groups: Arc<[Group]> = Arc::from(self.groups.clone());
        let flat: Arc<[Flat]> = Arc::from(self.flat.clone());
        let count = flat.len();
        let generation = self.generation;
        let cascade_open = self
            .last_page
            .is_some_and(|t| t.elapsed() < ROW_CASCADE_WINDOW)
            && scale > 0.0;
        let open: Rc<dyn Fn(&SymbolKey, &mut Window, &mut App)> = Rc::new(on_open);
        let toggle: Rc<dyn Fn(usize, &mut Window, &mut App)> = Rc::new(on_toggle);
        let row_h = ts.dense.line_height + sp.space_2;

        div()
            .id("symbol.refs")
            .v_flex()
            .size_full()
            .child(
                uniform_list("symbol.refs.list", count, move |range, _window, cx| {
                    let ext = cx.theme_ext();
                    let sp = ext.space;
                    let ts = ext.type_scale;
                    let colours = ext.colours;
                    let motion = MotionTokens::new(ext.motion_scale);

                    range
                        .map(|ix| {
                            let element = match flat[ix] {
                                Flat::Header(gx) => {
                                    let group = &groups[gx];
                                    let toggle = toggle.clone();
                                    div()
                                        .id(("symbol.refs.group", ix))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_1)
                                        .h(ts.dense.line_height + sp.space_2)
                                        .px(sp.space_2)
                                        .bg(colours.bg_raised)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .on_click(move |_, window, cx| toggle(gx, window, cx))
                                        .child(
                                            Icon::new(if group.collapsed {
                                                IconName::ChevronRight
                                            } else {
                                                IconName::ChevronDown
                                            })
                                            .text_color(colours.fg_faint)
                                            .with_size(gpui_component::Size::XSmall),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .overflow_hidden()
                                                .truncate()
                                                .font_family("monospace")
                                                .text_size(ts.dense.size)
                                                .line_height(ts.dense.line_height)
                                                .text_color(colours.fg_default)
                                                .child(group.file.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(ts.caption.size)
                                                .line_height(ts.caption.line_height)
                                                .text_color(colours.fg_faint)
                                                .child(group.count.clone()),
                                        )
                                }
                                Flat::Row { group: gx, row: rx } => {
                                    let row = &groups[gx].rows[rx];
                                    let key = row.target;
                                    let open = open.clone();
                                    let (badge_bg, badge_fg, badge_label) =
                                        precision_chrome(row, &colours);

                                    div()
                                        .id(("symbol.refs.row", ix))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_2)
                                        .h(ts.dense.line_height + sp.space_2)
                                        .pl(sp.space_5)
                                        .pr(sp.space_2)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .active(|s| s.bg(colours.bg_active))
                                        .on_click(move |_, window, cx| open(&key, window, cx))
                                        // line
                                        .child(
                                            div()
                                                .w(sp.space_6)
                                                .flex_shrink_0()
                                                .font_family("monospace")
                                                .text_size(ts.dense.size)
                                                .line_height(ts.dense.line_height)
                                                .text_color(colours.fg_faint)
                                                .children(row.line.clone()),
                                        )
                                        // path
                                        .child(
                                            div()
                                                .flex_1()
                                                .overflow_hidden()
                                                .truncate()
                                                .font_family("monospace")
                                                .text_size(ts.dense.size)
                                                .line_height(ts.dense.line_height)
                                                .text_color(colours.fg_muted)
                                                .child(row.path.clone()),
                                        )
                                        // kind
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(ts.caption.size)
                                                .line_height(ts.caption.line_height)
                                                .text_color(colours.fg_faint)
                                                .child(row.kind.clone()),
                                        )
                                        // precision badge
                                        .child(div().flex_shrink_0().child(Badge::custom(
                                            ("symbol.refs.precision", ix),
                                            badge_label,
                                            badge_bg,
                                            badge_fg,
                                        )))
                                }
                            };

                            // `row.cascade`, gated on the arrival window so
                            // scrolling never replays an entrance (§4.1).
                            if cascade_open {
                                row_enter(
                                    element,
                                    entrance_id("symbol.refs.enter", generation, ix),
                                    ix,
                                    &motion,
                                )
                            } else {
                                element.into_any_element()
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .flex_1(),
            )
            .into_any_element()
    }
}

fn project_ref(row: &RefRow) -> RefRowView {
    let raw = shared(&row.path);
    let (path, line) = split_line(&raw);
    let kind = shared(&row.kind_tag);
    RefRowView {
        target: row.target,
        path: SharedString::from(String::from(path)),
        line: line.map(|l| SharedString::from(String::from(l))),
        precision: Precision::classify(&kind),
        kind,
    }
}

/// Colours and label for the precision badge.
///
/// `Other` shows the engine's own words rather than being forced into one of
/// our two buckets — a claim we do not understand is still a claim (LD-7).
fn precision_chrome(
    row: &RefRowView,
    colours: &crate::theme::tokens::ColourRoles,
) -> (gpui::Hsla, gpui::Hsla, SharedString) {
    match row.precision {
        Precision::Precise => (
            colours.ok.opacity(0.18),
            colours.ok_fg,
            SharedString::from("precise"),
        ),
        Precision::Textual => (
            colours.warn.opacity(0.18),
            colours.warn_fg,
            SharedString::from("textual"),
        ),
        Precision::Other => (colours.bg_hover, colours.fg_muted, row.kind.clone()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ImplsTable
// ─────────────────────────────────────────────────────────────────────────────

/// One implementor row.
#[derive(Clone, Debug)]
struct ImplRowView {
    key: SymbolKey,
    label: SharedString,
}

/// The implementors table — flat, virtualized, same motion contract.
pub struct ImplsTable {
    rows: Vec<ImplRowView>,
    consumed_pages: usize,
    total: u64,
    total_text: SharedString,
    scroll: UniformListScrollHandle,
    generation: u64,
    last_page: Option<Instant>,
}

impl Default for ImplsTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ImplsTable {
    /// An empty table.
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            consumed_pages: 0,
            total: 0,
            total_text: SharedString::from("0"),
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            last_page: None,
        }
    }

    /// Drop everything for a new generation.
    pub fn reset(&mut self, generation: u64) {
        self.rows.clear();
        self.consumed_pages = 0;
        self.total = 0;
        self.total_text = SharedString::from("0");
        self.generation = generation;
        self.last_page = None;
    }

    /// Total implementors reported by the engine.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Pre-formatted total for the tab label.
    pub fn total_text(&self) -> SharedString {
        self.total_text.clone()
    }

    /// Consume any pages we have not projected yet.
    pub fn sync(&mut self, pages: &[ImplsPage]) -> bool {
        if self.consumed_pages >= pages.len() {
            return false;
        }
        for page in &pages[self.consumed_pages..] {
            if page.total != self.total {
                self.total = page.total;
                self.total_text = SharedString::from(page.total.to_string());
            }
            self.rows
                .extend(page.impls.iter().map(|r: &ImplRow| ImplRowView {
                    key: r.key,
                    label: shared(&r.label),
                }));
        }
        self.consumed_pages = pages.len();
        self.last_page = Some(Instant::now());
        true
    }

    /// Render the table, or a designed empty state.
    pub fn render(
        &self,
        streaming: bool,
        on_open: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        let ext = cx.theme_ext();
        let scale = ext.motion_scale;

        if self.rows.is_empty() {
            return if streaming {
                div().w_full().into_any_element()
            } else {
                EmptyState::new(
                    IconName::Frame,
                    SharedString::from("No implementors"),
                    SharedString::from("No type in the loaded corpus implements this."),
                    MotionTokens::new(scale),
                )
                .into_any_element()
            };
        }

        let rows: Arc<[ImplRowView]> = Arc::from(self.rows.clone());
        let count = rows.len();
        let generation = self.generation;
        let cascade_open = self
            .last_page
            .is_some_and(|t| t.elapsed() < ROW_CASCADE_WINDOW)
            && scale > 0.0;
        let open: Rc<dyn Fn(&SymbolKey, &mut Window, &mut App)> = Rc::new(on_open);

        div()
            .id("symbol.impls")
            .v_flex()
            .size_full()
            .child(
                uniform_list("symbol.impls.list", count, move |range, _window, cx| {
                    let ext = cx.theme_ext();
                    let sp = ext.space;
                    let ts = ext.type_scale;
                    let colours = ext.colours;
                    let motion = MotionTokens::new(ext.motion_scale);

                    range
                        .map(|ix| {
                            let row = &rows[ix];
                            let key = row.key;
                            let open = open.clone();
                            let element = div()
                                .id(("symbol.impls.row", ix))
                                .flex()
                                .flex_row()
                                .items_center()
                                .h(ts.dense.line_height + sp.space_2)
                                .px(sp.space_3)
                                .cursor_pointer()
                                .hover(|s| s.bg(colours.bg_hover))
                                .active(|s| s.bg(colours.bg_active))
                                .on_click(move |_, window, cx| open(&key, window, cx))
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .truncate()
                                        .font_family("monospace")
                                        .text_size(ts.mono.size)
                                        .line_height(ts.dense.line_height)
                                        .text_color(colours.fg_muted)
                                        .child(row.label.clone()),
                                );

                            if cascade_open {
                                row_enter(
                                    element,
                                    entrance_id("symbol.impls.enter", generation, ix),
                                    ix,
                                    &motion,
                                )
                            } else {
                                element.into_any_element()
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .flex_1(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(rows: &[(&str, &str)], total: u64) -> RefsPage {
        let refs: Vec<RefRow> = rows
            .iter()
            .map(|(path, kind)| RefRow {
                target: test_key(),
                path: (*path).into(),
                kind_tag: (*kind).into(),
            })
            .collect();
        RefsPage {
            refs: Arc::from(refs),
            total,
        }
    }

    fn test_key() -> SymbolKey {
        use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
        SymbolKey::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("t")),
            IntroId::from_raw([0u8; 32]),
        )
    }

    /// A trailing `:123` is a line number; anything else is part of the path.
    #[test]
    fn line_split_only_accepts_digits() {
        assert_eq!(split_line("src/a.rs:120"), ("src/a.rs", Some("120")));
        assert_eq!(split_line("src/a.rs"), ("src/a.rs", None));
        assert_eq!(split_line("C:/x/a.rs"), ("C:/x/a.rs", None));
        assert_eq!(split_line("src/a.rs:"), ("src/a.rs:", None));
    }

    /// Rows sharing a file collapse into one group in a single linear pass —
    /// no sorting anywhere.
    #[test]
    fn contiguous_rows_group_by_file() {
        let mut t = RefsTable::new();
        t.sync(&[page(
            &[
                ("src/a.rs:1", "precise"),
                ("src/a.rs:9", "precise"),
                ("src/b.rs:4", "textual"),
            ],
            3,
        )]);
        assert_eq!(t.groups.len(), 2);
        assert_eq!(&*t.groups[0].file, "src/a.rs");
        assert_eq!(t.groups[0].rows.len(), 2);
        assert_eq!(&*t.groups[0].count, "2");
        assert_eq!(&*t.groups[1].file, "src/b.rs");
        // 2 headers + 3 rows.
        assert_eq!(t.flat.len(), 5);
    }

    /// Collapsing a group removes its rows from the flat index but never the
    /// group header, so the reader can always get them back.
    #[test]
    fn collapsing_hides_rows_but_keeps_the_header() {
        let mut t = RefsTable::new();
        t.sync(&[page(&[("src/a.rs:1", "precise"), ("src/a.rs:2", "precise")], 2)]);
        assert_eq!(t.flat.len(), 3);
        assert!(t.toggle_group(0));
        assert_eq!(t.flat.len(), 1);
        assert!(t.toggle_group(0));
        assert_eq!(t.flat.len(), 3);
    }

    /// Toggling a group that does not exist is a no-op, not a panic.
    #[test]
    fn toggling_a_missing_group_is_a_no_op() {
        let mut t = RefsTable::new();
        assert!(!t.toggle_group(7));
    }

    /// Re-syncing the same pages twice must not duplicate rows — the store's
    /// `Progressive` is append-only and we track how much we have consumed.
    #[test]
    fn sync_is_idempotent_for_already_consumed_pages() {
        let mut t = RefsTable::new();
        let pages = vec![page(&[("src/a.rs:1", "precise")], 1)];
        assert!(t.sync(&pages));
        assert!(!t.sync(&pages), "no new pages, no work");
        assert_eq!(t.groups[0].rows.len(), 1);
    }

    /// A second page continuing the same file extends the existing group
    /// rather than opening a duplicate one.
    #[test]
    fn a_second_page_extends_the_open_group() {
        let mut t = RefsTable::new();
        let mut pages = vec![page(&[("src/a.rs:1", "precise")], 4)];
        assert!(t.sync(&pages));
        pages.push(page(&[("src/a.rs:2", "precise")], 4));
        assert!(t.sync(&pages));
        assert_eq!(t.groups.len(), 1);
        assert_eq!(t.groups[0].rows.len(), 2);
    }

    /// Precision is a claim, not a decoration: unknown tags stay unknown.
    #[test]
    fn precision_classification() {
        assert_eq!(Precision::classify("precise"), Precision::Precise);
        assert_eq!(Precision::classify("PRECISE"), Precision::Precise);
        assert_eq!(Precision::classify("textual"), Precision::Textual);
        assert_eq!(Precision::classify("call"), Precision::Other);
    }

    /// The tab's count string is built when a page lands, never in `render`.
    #[test]
    fn totals_are_preformatted_on_arrival() {
        let mut t = RefsTable::new();
        t.sync(&[page(&[("src/a.rs:1", "precise")], 128)]);
        assert_eq!(&*t.total_text(), "128");
        assert_eq!(t.total(), 128);
    }

    /// A reset clears everything and re-stamps the generation, so entrance ids
    /// from the previous generation cannot collide (LD-19).
    #[test]
    fn reset_restamps_the_generation() {
        let mut t = RefsTable::new();
        t.sync(&[page(&[("src/a.rs:1", "precise")], 1)]);
        t.reset(7);
        assert!(t.is_empty());
        assert_eq!(t.generation, 7);
        assert_eq!(t.consumed_pages, 0);
    }
}
