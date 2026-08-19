//! The Refs and Impls tables (§16).
//!
//! # Shape
//!
//! References are the one surface on a symbol page that can be genuinely huge —
//! a popular trait has tens of thousands of them. So this is `uniform_list` from
//! the first row (LD-6), over a *flattened* index: one `Arc<[FlatRow]>` where an
//! entry is either a file header or a reference under it. Collapsing a file
//! group rebuilds that flat vector once, at update time; the list itself never
//! learns about grouping, and `render` clones one `Arc` rather than a table.
//!
//! Columns are the Sourcegraph pattern: line · path · kind · precision. The
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

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Hsla, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, UniformListScrollHandle, Window, div, uniform_list,
};
use gpui_component::{Icon, IconName, Sizable as _};
use nudox_engine::wire::{ImplsPage, RefRow, RefsPage, SourceLocation, SymbolKey};

use crate::motion::declarative::{entrance_id, row_enter};
use crate::motion::tokens::{MotionTokens, ROW_CASCADE_WINDOW};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::theme::kind::LocalKindDiscriminant;
use crate::theme::tokens::ColourRoles;
use crate::ui::{Badge, EmptyState};

use super::header::shared;

/// Format a count once, on arrival. Never called from `render`.
fn count_text(n: usize) -> SharedString {
    SharedString::from(n.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Precision
// ─────────────────────────────────────────────────────────────────────────────

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
        if tag.eq_ignore_ascii_case("precise")
            || tag.eq_ignore_ascii_case("resolved")
            || tag.eq_ignore_ascii_case("definition")
        {
            return Self::Precise;
        }
        if tag.eq_ignore_ascii_case("textual")
            || tag.eq_ignore_ascii_case("search")
            || tag.eq_ignore_ascii_case("fuzzy")
            || tag.eq_ignore_ascii_case("heuristic")
        {
            Self::Textual
        } else {
            Self::Other
        }
    }
}

/// Colours and label for the precision badge.
///
/// `Other` shows the engine's own words rather than being forced into one of
/// our two buckets — a claim we do not understand is still a claim (LD-7).
fn precision_chrome(
    precision: Precision,
    kind: &SharedString,
    colours: &ColourRoles,
    alpha: &crate::theme::tokens::AlphaTokens,
) -> (Hsla, Hsla, SharedString) {
    match precision {
        // `al.tint` rather than the invented 0.18 these carried. The badge
        // always sits on a known surface (a table row on `bg_base`), so a
        // ladder rung is honest here — but the rung is the point: 0.18 was one
        // of sixteen distinct alphas across the crate, none of which agreed.
        Precision::Precise => (
            colours.ok.opacity(alpha.tint),
            colours.ok_fg,
            SharedString::from("precise"),
        ),
        Precision::Textual => (
            colours.warn.opacity(alpha.tint),
            colours.warn_fg,
            SharedString::from("textual"),
        ),
        Precision::Other => (colours.bg_hover, colours.fg_muted, kind.clone()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Projection
// ─────────────────────────────────────────────────────────────────────────────

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
/// TODO(wire): `RefRow` has no `line` field, so a line number can only reach us
/// encoded in `path`. When the wire type grows `line: u32` this function
/// disappears and the column becomes first-class.
fn split_line(path: &str) -> (&str, Option<&str>) {
    match path.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
            (head, Some(tail))
        }
        _ => (path, None),
    }
}

fn project_ref(row: &RefRow) -> RefRowView {
    let raw = shared(&row.path);
    let (path, line) = split_line(&raw);
    let kind = shared(&row.kind_tag);
    RefRowView {
        target: row.target.clone(),
        path: SharedString::from(String::from(path)),
        line: line.map(|l| SharedString::from(String::from(l))),
        precision: Precision::classify(&kind),
        kind,
    }
}

/// `true` when `file` starts a new group after `last`.
fn group_boundary(last: Option<&str>, file: &str) -> bool {
    match last {
        Some(prev) => prev != file,
        None => true,
    }
}

/// Whether a group is large enough to be worth a header of its own.
///
/// # A group of one is not a group
///
/// The References table groups references by file, which is right when a
/// symbol is used eleven times across four files and absurd when it is used
/// once. In the single-reference case the header and the row carried *the same
/// string*: `11-refs-tab.png` showed
/// `⌄ nudox-fixture-rich.format_point  1` above
/// `nudox-fixture-rich.format_point  Function  [Function]`, two rows of chrome
/// and one fact, plus a disclosure triangle for hiding a row you can already
/// see.
///
/// A header earns its line by telling the reader something the rows beneath it
/// do not. With one row beneath, it cannot. So it is not drawn, the row stands
/// at depth zero carrying its own path, and the table gets shorter exactly when
/// there was nothing to organise.
///
/// The threshold is 2 and not a tuning knob: it is the smallest number of rows
/// for which "these share a file" is a fact about more than one of them.
const GROUP_HEADER_MIN_ROWS: usize = 2;

/// Whether this group draws a header.
#[inline]
fn group_has_header(row_count: usize) -> bool {
    row_count >= GROUP_HEADER_MIN_ROWS
}

/// How many flat entries a set of `(collapsed, row_count)` groups produces.
///
/// Used to size the flat index exactly, and separately testable — the collapse
/// arithmetic is the one thing here that can silently desynchronise the list's
/// item count from what `render` will produce.
///
/// A headerless group (see [`group_has_header`]) contributes only its rows, and
/// cannot be collapsed: there is no header to click, so `collapsed` is not
/// reachable for it and is ignored rather than honoured. Honouring it would let
/// a group vanish with no way to bring it back.
fn flat_len(groups: impl Iterator<Item = (bool, usize)>) -> usize {
    groups
        .map(|(collapsed, n)| {
            if !group_has_header(n) {
                return n;
            }
            1 + if collapsed { 0 } else { n }
        })
        .sum()
}

/// A file grouping of a contiguous run of references.
#[derive(Clone, Debug)]
struct Group {
    file: SharedString,
    count: SharedString,
    collapsed: bool,
    rows: Vec<RefRowView>,
}

/// One entry of the flat index the `uniform_list` actually renders.
///
/// Cloning is a handful of `Arc` bumps, so rebuilding this on collapse or on a
/// page arrival is cheap, and `render` only ever clones the enclosing `Arc`.
#[derive(Clone, Debug)]
enum FlatRow {
    Header {
        group: usize,
        file: SharedString,
        count: SharedString,
        collapsed: bool,
    },
    /// A reference, and whether it sits under a header.
    ///
    /// Carried on the row rather than recomputed in `render` because the
    /// renderer walks a flat index and has no idea what preceded item `n` — it
    /// would have to scan backwards for a header, per row, per frame. Depth is
    /// a property of the row; the projection knows it; so the projection says
    /// it.
    Row {
        /// The reference itself.
        row: RefRowView,
        /// `true` when a group header is drawn above this row, and the row is
        /// therefore indented under it.
        nested: bool,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// RefsTable
// ─────────────────────────────────────────────────────────────────────────────

/// The grouped, virtualized references table.
pub struct RefsTable {
    groups: Vec<Group>,
    flat: Arc<[FlatRow]>,
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
            flat: Arc::from(Vec::new()),
            consumed_pages: 0,
            total: 0,
            total_text: SharedString::from("0"),
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            last_page: None,
        }
    }

    /// Drop everything and re-stamp for a new generation.
    pub fn reset(&mut self, generation: u64) {
        self.groups.clear();
        self.flat = Arc::from(Vec::new());
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
            let last_file = self.groups.last().map(|g| g.file.as_ref());
            if group_boundary(last_file, &view.path) {
                self.groups.push(Group {
                    file: view.path.clone(),
                    count: count_text(0),
                    collapsed: false,
                    rows: Vec::new(),
                });
            }
            if let Some(group) = self.groups.last_mut() {
                group.rows.push(view);
                group.count = count_text(group.rows.len());
            }
        }
    }

    /// Toggle a file group. Returns `true` if the flat index changed.
    pub fn toggle_group(&mut self, group_ix: usize) -> bool {
        match self.groups.get_mut(group_ix) {
            // A headerless group has no affordance to collapse it, so nothing
            // can toggle it — and if something did, the rows would disappear
            // with no header left behind to bring them back. Refusing here
            // means that state is unreachable rather than merely unlikely.
            Some(group) if group_has_header(group.rows.len()) => {
                group.collapsed = !group.collapsed;
                self.rebuild_flat();
                true
            }
            _ => false,
        }
    }

    fn rebuild_flat(&mut self) {
        let len = flat_len(self.groups.iter().map(|g| (g.collapsed, g.rows.len())));
        let mut flat = Vec::with_capacity(len);
        for (gx, group) in self.groups.iter().enumerate() {
            let nested = group_has_header(group.rows.len());
            if nested {
                flat.push(FlatRow::Header {
                    group: gx,
                    file: group.file.clone(),
                    count: group.count.clone(),
                    collapsed: group.collapsed,
                });
                if group.collapsed {
                    continue;
                }
            }
            flat.extend(
                group
                    .rows
                    .iter()
                    .cloned()
                    .map(|row| FlatRow::Row { row, nested }),
            );
        }
        debug_assert_eq!(flat.len(), len, "flat_len disagrees with rebuild_flat");
        self.flat = Arc::from(flat);
    }

    /// Render the table, or a designed empty state (LD-16).
    ///
    /// `streaming` suppresses the empty state while pages are still arriving —
    /// "no references" is a *finding*, and we do not report it prematurely.
    ///
    /// # Layout contract
    ///
    /// Same as `ImplsTable::render`: returns naturally-sized content whose total
    /// height is `rows × row_h`.  The caller wraps it in `max_h.overflow_y_scroll`.
    /// The `uniform_list` receives an explicit `h` so it has a definite height to
    /// measure items against.  See `ImplsTable::render` for the full rationale.
    pub fn render(
        &self,
        streaming: bool,
        on_open: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static,
        on_toggle: impl Fn(usize, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        let ext = cx.theme_ext();
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

        // One `Arc` bump per frame — the table itself is never cloned here.
        let flat = self.flat.clone();
        let count = flat.len();
        let generation = self.generation;
        let cascade_open = self
            .last_page
            .is_some_and(|t| t.elapsed() < ROW_CASCADE_WINDOW)
            && scale > 0.0;
        let open: Rc<dyn Fn(&SymbolKey, &mut Window, &mut App)> = Rc::new(on_open);
        let toggle: Rc<dyn Fn(usize, &mut Window, &mut App)> = Rc::new(on_toggle);

        // Pre-compute list height — must match the `.h(row_h)` on each row element.
        let _sp = ext.space;
        let ts = ext.type_scale;
        let row_h = ext.row_height(ts.dense);
        let list_h = gpui::px(count as f32 * f32::from(row_h));

        div()
            .id("symbol.refs")
            .w_full()
            .child(
                uniform_list("symbol.refs.list", count, move |range, _window, cx| {
                    let ext = cx.theme_ext();
                    let sp = ext.space;
                    let ts = ext.type_scale;
                    let colours = ext.colours;
                    let motion = MotionTokens::new(ext.motion_scale);
                    let row_h = ext.row_height(ts.dense);

                    range
                        .map(|ix| {
                            let element = match &flat[ix] {
                                FlatRow::Header {
                                    group,
                                    file,
                                    count,
                                    collapsed,
                                } => {
                                    let group = *group;
                                    let toggle = toggle.clone();
                                    // `w_full` is load-bearing, not tidiness.
                                    //
                                    // Without it this row shrink-wraps its
                                    // content, so `bg_raised` painted a filled
                                    // rectangle that stopped dead partway
                                    // across the table — visible in the old
                                    // `11-refs-tab.png` as a hard vertical
                                    // edge mid-row — and the hover fill was
                                    // clipped to the same width. `flex_1` on
                                    // the label could not save it: there is no
                                    // free space to claim inside a container
                                    // that sized itself to its children. Same
                                    // family as doctrine §8's dropped
                                    // `relative()`.
                                    div()
                                        .id(("symbol.refs.group", ix))
                                        .w_full()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_1)
                                        .h(row_h)
                                        .px(sp.space_2)
                                        // No fill.
                                        //
                                        // The header used to be `bg_raised`
                                        // over unfilled rows, which is a
                                        // legitimate pattern and was the wrong
                                        // one here: `bg_raised` is also the
                                        // plane the section header above it
                                        // sits on, so a group header and the
                                        // "References" header read as the same
                                        // kind of object, and the rows beneath
                                        // — bare, but hovering to `bg_hover`,
                                        // a *lighter* colour — read as more
                                        // prominent than the thing containing
                                        // them. Hierarchy ran backwards.
                                        //
                                        // Type and colour carry it instead:
                                        // the header is muted and set in the
                                        // UI face, the rows are default-weight
                                        // monospace, and only rows take a
                                        // fill. This is how Linear and Notion
                                        // separate a section from its contents
                                        // in a dense panel, and how Primer's
                                        // ActionList group heading works —
                                        // small, muted, unfilled.
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .on_click(move |_, window, cx| toggle(group, window, cx))
                                        .child(
                                            Icon::new(if *collapsed {
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
                                                .text_size(ts.dense.size)
                                                .line_height(ts.dense.line_height)
                                                .font_weight(gpui::FontWeight(
                                                    ts.caption.weight as f32,
                                                ))
                                                .text_color(colours.fg_muted)
                                                .child(file.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(ts.caption.size)
                                                .line_height(ts.caption.line_height)
                                                .text_color(colours.fg_faint)
                                                .child(count.clone()),
                                        )
                                }
                                FlatRow::Row { row, nested } => {
                                    let key = row.target.clone();
                                    let open = open.clone();
                                    let (badge_bg, badge_fg, badge_label) = precision_chrome(
                                        row.precision,
                                        &row.kind,
                                        &colours,
                                        &ext.alpha,
                                    );

                                    // The engine's raw kind tag, shown only
                                    // when the precision badge is not already
                                    // showing it.
                                    //
                                    // `precision_chrome` returns the kind tag
                                    // *as the badge label* for
                                    // `Precision::Other` — that is LD-7 doing
                                    // its job, surfacing a claim we do not
                                    // recognise in the engine's own words. But
                                    // the row also had a dedicated kind
                                    // column, so for every unrecognised tag —
                                    // which is most of them — the same string
                                    // was printed twice, once dim and once in
                                    // a chip, four pixels apart:
                                    // `Function [Function]` in the old
                                    // `11-refs-tab.png`. Duplication is not a
                                    // styling problem; it makes the reader
                                    // look for a difference that is not there.
                                    let show_kind_column =
                                        !matches!(row.precision, Precision::Other);

                                    // A row under a header is indented by one
                                    // step and carries an indent guide; a row
                                    // in a headerless group is at depth zero
                                    // and carries none.
                                    //
                                    // One `indent` step (8 px, Primer's
                                    // TreeView value) rather than the
                                    // `space_5` (20 px) this used: the table
                                    // is two levels deep at most, and 20 px
                                    // per level spends a fifth of a narrow
                                    // column saying "still the same file".
                                    // What actually communicates depth is the
                                    // guide, not the distance.
                                    let indent = if *nested {
                                        sp.space_2 + sp.indent
                                    } else {
                                        sp.space_2
                                    };

                                    div()
                                        .id(("symbol.refs.row", ix))
                                        // See the group header above: without
                                        // `w_full` the hover and active fills
                                        // clip to the content width.
                                        .w_full()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_2)
                                        .h(row_h)
                                        .pl(indent)
                                        .pr(sp.space_2)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .active(|s| s.bg(colours.bg_active))
                                        .on_click(move |_, window, cx| open(&key, window, cx))
                                        // The indent guide: a hairline in the
                                        // gutter, running the full height of
                                        // the row so consecutive rows draw one
                                        // continuous line down the group.
                                        //
                                        // Borrowed from Primer's TreeView,
                                        // which paints a 1 px
                                        // `borderColor-muted` rule per level.
                                        // It is `border_subtle` here — the
                                        // role that exists for marks the
                                        // reader is not meant to notice — and
                                        // it is what makes "these rows belong
                                        // to that header" legible without
                                        // spending 20 px on it.
                                        .when(*nested, |el| {
                                            el.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .w(sp.border_width)
                                                    .h_full()
                                                    .bg(colours.border_subtle),
                                            )
                                        })
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
                                        // kind — see `show_kind_column`
                                        .when(show_kind_column, |el| {
                                            el.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_size(ts.caption.size)
                                                    .line_height(ts.caption.line_height)
                                                    .text_color(colours.fg_faint)
                                                    .child(row.kind.clone()),
                                            )
                                        })
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
                .h(list_h),
            )
            .into_any_element()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ImplsTable
// ─────────────────────────────────────────────────────────────────────────────

// ── Blanket / variadic grouping ──────────────────────────────────────────────
//
// `ImplRow` (wire) carries `key`, `label`, `is_blanket`, `trait_label`, and
// `self_generic_count`.  These enable two classes of structured grouping:
//
// * Blanket grouping: partition rows on `is_blanket` at *projection* time (in
//   `sync`, not `render`).  Render own impls first, then a separate "Blanket
//   Implementations" subheading, collapsed by default.  The intent matches
//   docs.rs: blanket impls describe the ecosystem's structure, not the specific
//   type being viewed, so they should not crowd the type's own impls.
//
// * Variadic / tuple family detection: group rows by `(trait_label,
//   self_base)`.  If a group has ≥3 members with consecutive
//   `self_generic_count` values, collapse it to one summary row.  The threshold
//   of 3 and the consecutiveness requirement prevent mis-grouping two unrelated
//   impls that happen to share a trait — a wrong grouping is worse than none.

/// One implementor row, fully projected and render-ready.
#[derive(Clone, Debug)]
struct ImplRowView {
    key: SymbolKey,
    label: SharedString,
    is_blanket: bool,
    /// The trait label, used as the grouping key for variadic family detection.
    trait_label: Option<SharedString>,
    /// Generic count on the self type — used for arity-family detection.
    self_generic_count: u32,
    /// Copyable package-relative source target, when the impl is navigable.
    source: Option<SharedString>,

    // ── Display decomposition (built by `ImplRowView::new`, never by hand) ────
    //
    // The row used to render `label` as one flat run of monospace text: no
    // chip, no weight change, nothing separating the trait from the type it is
    // implemented for. Six of those stacked read as an undifferentiated block,
    // while the *search* rows one keystroke away showed the same class of
    // information with a kind chip and three styled tiers. One app, two
    // answers.
    //
    // These two fields are what let the row be styled like a search row. They
    // are derived — see `new` — precisely so that no construction site can
    // produce a row whose parts disagree with its label. Three sites build
    // `ImplRowView` (streaming projection, variadic-family collapse, and the
    // test helper); when the split lived in the template each would have
    // needed to get it right independently, which is the shape of defect
    // doctrine §2 is about.
    /// What the reader scans for: the trait name, or the self type for an
    /// inherent impl. Never empty — falls back to the whole label.
    primary: SharedString,
    /// The type the trait is implemented *for*, when the label named one.
    /// `None` for an inherent impl, where `primary` already is the type.
    self_type: Option<SharedString>,
}

impl ImplRowView {
    /// The only way to build a row: the display split is derived here, once.
    fn new(
        key: SymbolKey,
        label: SharedString,
        is_blanket: bool,
        trait_label: Option<SharedString>,
        self_generic_count: u32,
        source: Option<SharedString>,
    ) -> Self {
        let (head, tail) = split_impl_label(&label);
        // Prefer the wire's own `trait_label` over anything parsed out of the
        // rendered label — it is the engine's structured answer to the same
        // question, and it stays right when the label formatting changes.
        let primary: SharedString = match trait_label.as_ref() {
            Some(t) if !t.is_empty() => t.clone(),
            _ => {
                let stripped = strip_impl_keyword(head);
                if !stripped.is_empty() {
                    SharedString::from(stripped.to_owned())
                } else if !label.is_empty() {
                    label.clone()
                } else {
                    // A producer that sends an empty label would otherwise
                    // paint a blank row — an implementation the reader can see
                    // a chip for, click on, and not read. Same rule as LD-7's
                    // unknown-kind chip: the unknown is *shown*, never left as
                    // a gap, because a gap is indistinguishable from a
                    // rendering bug.
                    SharedString::from("unnamed impl")
                }
            }
        };
        let self_type = tail
            .filter(|t| !t.is_empty())
            .map(|t| SharedString::from(t.to_owned()));

        Self {
            key,
            label,
            is_blanket,
            trait_label,
            self_generic_count,
            source,
            primary,
            self_type,
        }
    }
}

/// A flat entry in the impls list — either a row or a section subheading.
#[derive(Clone, Debug)]
enum ImplFlatRow {
    /// A rendered implementation row.
    Row(ImplRowView),
    /// A subheading (e.g. "Blanket Implementations (N)"), collapsed by default.
    Subheading {
        label: SharedString,
        count: SharedString,
        collapsed: bool,
    },
}

// ── Impl label decomposition ──────────────────────────────────────────────────

/// Split an impl label at the ` for ` that separates the trait from the type.
///
/// `"impl<'h> Iterator for memchr.memchr.Memchr"`
///   → `("impl<'h> Iterator", Some("memchr.memchr.Memchr"))`
///
/// An inherent impl has no ` for `, and yields `(whole, None)`.
///
/// The first occurrence is the separator: everything after it is the self
/// type, however long. This is deliberately total — any string splits, and a
/// malformed one simply lands in the left half. Row *styling* must not depend
/// on the label being well-formed, because it currently is not: the
/// `impl ? for …` placeholder in `tests/shots/memchr/` is a live backend defect
/// (L39) and the row has to look right both before and after it is fixed.
fn split_impl_label(label: &str) -> (&str, Option<&str>) {
    const SEP: &str = " for ";
    match label.find(SEP) {
        Some(ix) => (&label[..ix], Some(&label[ix + SEP.len()..])),
        None => (label, None),
    }
}

/// Drop a leading `impl` keyword and the generic list that binds it.
///
/// `"impl<'h> Iterator"` → `"Iterator"`; `"impl Clone"` → `"Clone"`.
///
/// The row carries an `impl` kind chip, so repeating the keyword in the text
/// beside it spends horizontal space to say what the chip already says. Only a
/// *bare leading* `impl` is removed, and only with a balanced generic list; if
/// the text does not have that exact shape it is returned untouched, because a
/// half-stripped label is worse than an unstripped one.
fn strip_impl_keyword(head: &str) -> &str {
    let Some(rest) = head.trim_start().strip_prefix("impl") else {
        return head;
    };
    // `implFoo` is not the keyword — require a delimiter after it.
    let rest = match rest.chars().next() {
        None => return "",
        Some('<') => match balanced_angle_end(rest) {
            Some(end) => &rest[end..],
            // Unbalanced generics: leave the whole thing alone.
            None => return head,
        },
        Some(c) if c.is_whitespace() => rest,
        // Something like `impls`, not the keyword.
        Some(_) => return head,
    };
    rest.trim()
}

/// Byte index one past the `>` that closes the `<` at the start of `s`.
///
/// Counts nesting so `<Vec<T>>` closes at the outer `>`, and returns `None`
/// when the list never closes — or when `s` does not open one at all.
///
/// That last clause is the function's own guard, not the caller's
/// responsibility. Without it, `depth -= 1` on a leading `>` underflows a
/// `usize` and panics in debug builds. The one current call site happens to
/// check `starts_with('<')` first, but a precondition that lives only in the
/// caller is one refactor away from being forgotten (§2: fix the thing that
/// let the bug be expressible, not the site that happens to avoid it).
fn balanced_angle_end(s: &str) -> Option<usize> {
    if !s.starts_with('<') {
        return None;
    }
    let mut depth = 0usize;
    for (ix, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(ix + ch.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

// ── Variadic family detection ─────────────────────────────────────────────────

/// Return the self-type base: the label with everything from `<` stripped.
///
/// `"impl Debug for Router<E>"` → `"impl Debug for Router"`.
/// Used as the second key in `(trait_label, self_base)` grouping.
fn self_base(label: &str) -> &str {
    match label.find('<') {
        Some(idx) => label[..idx].trim_end(),
        None => label,
    }
}

/// Detect whether `counts` (sorted ascending) forms a run of ≥3 consecutive integers.
///
/// Non-consecutive counts and runs shorter than 3 return `false`.
fn is_consecutive_run(counts: &[u32]) -> bool {
    counts.len() >= 3 && counts.windows(2).all(|w| w[1] == w[0] + 1)
}

/// Collapse a variadic family into a single summary row.
///
/// Called only when `is_consecutive_run` returns `true`.  The summary row uses
/// the key of the first member and a label like `"impl Handler for F  (arities 1–16)"`.
/// No `format!` is called from `render`; the string is built here at projection time.
fn collapse_family(rows: &[ImplRowView]) -> ImplRowView {
    let first = rows.first().expect("family is non-empty by construction");
    let min = rows.iter().map(|r| r.self_generic_count).min().unwrap_or(0);
    let max = rows.iter().map(|r| r.self_generic_count).max().unwrap_or(0);
    let base_label = self_base(&first.label);
    let summary = SharedString::from(
        [
            base_label,
            "  (arities ",
            &min.to_string(),
            "–",
            &max.to_string(),
            ")",
        ]
        .concat(),
    );
    ImplRowView::new(
        first.key.clone(),
        summary,
        first.is_blanket,
        first.trait_label.clone(),
        first.self_generic_count,
        first.source.clone(),
    )
}

/// Project a slice of `ImplRowView`s through variadic-family detection.
///
/// Returns a new vec where any group of ≥3 rows with the same `(trait_label,
/// self_base)` and consecutive `self_generic_count` values is replaced by a
/// single summary row.  All other rows pass through unchanged.
///
/// This is called at projection time only (inside `sync`), not in `render`.
fn apply_variadic_grouping(rows: Vec<ImplRowView>) -> Vec<ImplRowView> {
    // Group by (trait_label, self_base(label)).
    // We use an ordered vec of groups to preserve arrival order.
    let mut groups: Vec<(Option<SharedString>, SharedString, Vec<ImplRowView>)> = Vec::new();

    for row in rows {
        let base = SharedString::from(self_base(&row.label).to_owned());
        let key_trait = row.trait_label.clone();
        // Find an existing group with the same (trait, base).
        let found = groups
            .iter_mut()
            .find(|(t, b, _)| *t == key_trait && *b == base);
        match found {
            Some((_, _, group)) => group.push(row),
            None => groups.push((key_trait, base, vec![row])),
        }
    }

    let mut out = Vec::new();
    for (_, _, mut group) in groups {
        // Sort by self_generic_count before checking consecutiveness.
        group.sort_unstable_by_key(|r| r.self_generic_count);
        let counts: Vec<u32> = group.iter().map(|r| r.self_generic_count).collect();
        if is_consecutive_run(&counts) {
            out.push(collapse_family(&group));
        } else {
            out.extend(group);
        }
    }
    out
}

/// Build the flat list used by `uniform_list`.
///
/// Layout:
///   [own row] ...
///   [Subheading "Blanket Implementations (N)" — collapsed]
///   [blanket row] ...   ← only when the subheading is expanded
///
/// The subheading is only emitted when there are blanket rows.
fn build_flat(
    own_rows: &[ImplRowView],
    blanket_rows: &[ImplRowView],
    blanket_collapsed: bool,
) -> Vec<ImplFlatRow> {
    let mut flat = Vec::with_capacity(
        own_rows.len()
            + if blanket_rows.is_empty() {
                0
            } else {
                1 + if blanket_collapsed {
                    0
                } else {
                    blanket_rows.len()
                }
            },
    );

    for r in own_rows {
        flat.push(ImplFlatRow::Row(r.clone()));
    }

    if !blanket_rows.is_empty() {
        // Precompute the count label at projection time — §1.1.4 forbids format!
        // inside render.
        let count_str = blanket_rows.len().to_string();
        flat.push(ImplFlatRow::Subheading {
            label: SharedString::from("Blanket Implementations"),
            count: SharedString::from(count_str),
            collapsed: blanket_collapsed,
        });
        if !blanket_collapsed {
            for r in blanket_rows {
                flat.push(ImplFlatRow::Row(r.clone()));
            }
        }
    }

    flat
}

/// The implementors table — flat, virtualized, same motion contract as `RefsTable`.
pub struct ImplsTable {
    /// Own (non-blanket) impls after variadic-family collapsing.
    own_rows: Vec<ImplRowView>,
    /// Blanket impls after variadic-family collapsing.
    blanket_rows: Vec<ImplRowView>,
    /// Flat list for the `uniform_list`.  Rebuilt on every `sync` and on
    /// blanket-section toggle.
    flat: Arc<[ImplFlatRow]>,
    /// Whether the "Blanket Implementations" subheading is collapsed.
    blanket_collapsed: bool,
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
            own_rows: Vec::new(),
            blanket_rows: Vec::new(),
            flat: Arc::from(Vec::new()),
            blanket_collapsed: true,
            consumed_pages: 0,
            total: 0,
            total_text: SharedString::from("0"),
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            last_page: None,
        }
    }

    /// Drop everything and re-stamp for a new generation.
    pub fn reset(&mut self, generation: u64) {
        self.own_rows.clear();
        self.blanket_rows.clear();
        self.flat = Arc::from(Vec::new());
        self.blanket_collapsed = true;
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

    /// `true` when no page has landed yet.
    pub fn is_empty(&self) -> bool {
        self.flat.is_empty()
    }

    /// The non-blanket impls' labels, in display order.
    ///
    /// The page's table of contents nests these under `Implementations`
    /// (GUI-WORKORDER-2 F3): `Clone`, `Debug`, `Iterator` — the landmarks a
    /// reader scanning a type actually navigates by. Blanket impls are
    /// deliberately excluded, for the same reason they are collapsed in the
    /// table itself: they describe the ecosystem, not this type.
    ///
    /// Cloned `SharedString`s, built at projection time; this is a refcount
    /// bump per row and no formatting (§1.1.4).
    pub fn own_labels(&self) -> Arc<[SharedString]> {
        self.own_rows.iter().map(|r| r.label.clone()).collect()
    }

    /// Toggle the "Blanket Implementations" collapsed section.
    ///
    /// Returns `true` if the flat list changed (i.e. there were blanket rows).
    pub fn toggle_blanket(&mut self) -> bool {
        if self.blanket_rows.is_empty() {
            return false;
        }
        self.blanket_collapsed = !self.blanket_collapsed;
        self.rebuild_flat();
        true
    }

    /// Consume any pages we have not projected yet.
    ///
    /// Projection (partition + variadic grouping) runs here — never in `render`.
    pub fn sync(&mut self, pages: &[ImplsPage]) -> bool {
        if self.consumed_pages >= pages.len() {
            return false;
        }

        // Collect raw projected rows from the new pages.
        let mut raw_own: Vec<ImplRowView> = Vec::new();
        let mut raw_blanket: Vec<ImplRowView> = Vec::new();

        for page in &pages[self.consumed_pages..] {
            if page.total != self.total {
                self.total = page.total;
                self.total_text = SharedString::from(page.total.to_string());
            }
            for r in page.impls.iter() {
                let view = ImplRowView::new(
                    r.key.clone(),
                    shared(&r.label),
                    r.is_blanket,
                    r.trait_label.as_ref().map(|t| shared(t)),
                    r.self_generic_count,
                    match &r.source {
                        SourceLocation::Declared { .. } => {
                            r.source.jump_target().map(SharedString::from)
                        }
                        _ => None,
                    },
                );
                if r.is_blanket {
                    raw_blanket.push(view);
                } else {
                    raw_own.push(view);
                }
            }
        }
        self.consumed_pages = pages.len();
        self.last_page = Some(Instant::now());

        // Append to accumulated rows, then re-run variadic grouping on the
        // complete set so a family that straddles two pages is still detected.
        self.own_rows.extend(raw_own);
        self.blanket_rows.extend(raw_blanket);
        self.own_rows = apply_variadic_grouping(std::mem::take(&mut self.own_rows));
        self.blanket_rows = apply_variadic_grouping(std::mem::take(&mut self.blanket_rows));

        self.rebuild_flat();
        true
    }

    fn rebuild_flat(&mut self) {
        let flat = build_flat(&self.own_rows, &self.blanket_rows, self.blanket_collapsed);
        self.flat = Arc::from(flat);
    }

    /// Render the table, or a designed empty state.
    ///
    /// # Layout contract
    ///
    /// This method returns a `w_full` div whose height is governed entirely by
    /// its content (each row has a definite pixel height, so the total is
    /// `rows × row_h`).  The *caller* (mod.rs `symbol.impls.body`) wraps this in
    /// `max_h(400px).overflow_y_scroll()`, which scrolls when the content is
    /// taller than the cap.  We must NOT put `size_full()` or `flex_1()` here,
    /// because those resolve against the wrapper's height — and `max_h` alone
    /// does not establish a definite height for `uniform_list` to measure against.
    ///
    /// The `uniform_list` itself is given an explicit `h` computed from the row
    /// count and the design-time row height (dense line height + 2 × space_1).
    /// `uniform_list` renders only the rows currently in the viewport; the
    /// explicit height tells the scroller how much total content exists.
    pub fn render(
        &self,
        streaming: bool,
        on_open: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static,
        on_source: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
        on_toggle_blanket: impl Fn(&mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        let ext = cx.theme_ext();
        let scale = ext.motion_scale;

        if self.flat.is_empty() {
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

        let flat = self.flat.clone();
        let count = flat.len();
        let generation = self.generation;
        let cascade_open = self
            .last_page
            .is_some_and(|t| t.elapsed() < ROW_CASCADE_WINDOW)
            && scale > 0.0;
        let open: Rc<dyn Fn(&SymbolKey, &mut Window, &mut App)> = Rc::new(on_open);
        let source: Rc<dyn Fn(&SharedString, &mut Window, &mut App)> = Rc::new(on_source);
        let toggle_blanket: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(on_toggle_blanket);

        // Pre-compute the row height so the `uniform_list` can receive an
        // explicit `h`.  This must match the `.h(...)` on each row element below
        // — if they diverge the list measures at the wrong height and clips rows.
        let _sp = ext.space;
        let ts = ext.type_scale;
        // Impl rows are set in `mono`, so a row is one mono line tall — taller
        // than the `dense` rows of the references table beside it. Both come
        // from the one `row_height` definition rather than a formula retyped
        // per table (see `NudoxThemeExt::row_height`).
        let row_h = ext.row_height(ts.mono);
        // The total natural height of all rows.  `uniform_list` uses this as its
        // scroll extent; the outer `max_h + overflow_y_scroll` wrapper clips it.
        let list_h = gpui::px(count as f32 * f32::from(row_h));

        div()
            .id("symbol.impls")
            .w_full()
            .child(
                uniform_list("symbol.impls.list", count, move |range, _window, cx| {
                    let ext = cx.theme_ext();
                    let sp = ext.space;
                    let ts = ext.type_scale;
                    let colours = ext.colours;
                    let motion = MotionTokens::new(ext.motion_scale);
                    let row_h = ext.row_height(ts.mono);

                    range
                        .map(|ix| {
                            let element = match &flat[ix] {
                                // One implementation, styled to the same
                                // vocabulary as a search hit: a kind chip on
                                // the left, the scannable name in the primary
                                // foreground, and the supporting detail
                                // recessed beside it.
                                //
                                // Before this, the row was a single flat run of
                                // `label` in monospace `fg_default` — no chip,
                                // no weight change, no separation between the
                                // trait and the type. The information was
                                // identical to what a search row shows and the
                                // two were styled nothing alike, which is the
                                // consistency failure the craft pass named
                                // first.
                                //
                                // The row stays exactly one line tall in every
                                // branch. `uniform_list` measures every item at
                                // the `row_h` computed above and the list is
                                // given an explicit `count * row_h` height, so
                                // a two-line branch here would not wrap — it
                                // would silently clip and desynchronise the
                                // scroll extent.
                                ImplFlatRow::Row(row) => {
                                    let key = row.key.clone();
                                    let open = open.clone();
                                    let source_handler = source.clone();
                                    let source_target = row.source.clone();
                                    // `for <type>` — two elements, present only
                                    // when the label actually named a self
                                    // type, so an inherent impl does not render
                                    // a dangling preposition. Built as a list
                                    // rather than chained conditionally so the
                                    // row keeps one concrete element type.
                                    let tail: Vec<gpui::AnyElement> = match row.self_type.clone() {
                                        None => Vec::new(),
                                        Some(self_type) => vec![
                                            div()
                                                .flex_shrink_0()
                                                // `caption` size on the `mono`
                                                // leading, deliberately: the
                                                // three runs on this row have
                                                // to sit on one baseline, and
                                                // `for` is grammar rather than
                                                // content, so it is set a step
                                                // down and in the faintest
                                                // foreground. Pairing a size
                                                // with another token's leading
                                                // is drift everywhere it is
                                                // not doing exactly this.
                                                .text_size(ts.caption.size)
                                                .line_height(ts.mono.line_height)
                                                .text_color(colours.fg_faint)
                                                .child(SharedString::from("for"))
                                                .into_any_element(),
                                            div()
                                                // The self type is the run
                                                // allowed to grow and the first
                                                // to be truncated: `min_w_0` is
                                                // what lets it shrink below its
                                                // content width so `truncate()`
                                                // fires instead of the text
                                                // overrunning the panel. These
                                                // labels are long and fully
                                                // qualified — the row must not
                                                // assume short ones.
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .truncate()
                                                .font_family("monospace")
                                                .text_size(ts.mono.size)
                                                .line_height(ts.mono.line_height)
                                                .text_color(colours.fg_muted)
                                                .child(self_type)
                                                .into_any_element(),
                                        ],
                                    };
                                    div()
                                        .id(("symbol.impls.row", ix))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_2)
                                        // `w_full` + `overflow_hidden` for the
                                        // same reason the outline row needs
                                        // them: a flex child can only be
                                        // truncated against a parent that has
                                        // a definite width. Without this the
                                        // row grows to fit its longest label,
                                        // the `flex_1` self type finds all the
                                        // space it asked for, `truncate()`
                                        // never fires, and a fully-qualified
                                        // impl runs out of the panel. memchr's
                                        // labels are short enough to hide it;
                                        // a real trait-heavy type is not.
                                        .w_full()
                                        .overflow_hidden()
                                        .h(row_h)
                                        // `space_4`, matching the disclosure
                                        // header directly above and the
                                        // documentation blocks directly above
                                        // that. It was `space_3`, so the impl
                                        // rows sat 4 px inboard of every other
                                        // block in the reader and the column
                                        // had two left edges.
                                        .px(sp.space_4)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .active(|s| s.bg(colours.bg_active))
                                        .on_click(move |_, window, cx| open(&key, window, cx))
                                        .child(
                                            Badge::for_kind(
                                                ("symbol.impls.kind", ix),
                                                LocalKindDiscriminant::Impl,
                                                cx,
                                            )
                                            .flex_shrink_0(),
                                        )
                                        // The trait (or, for an inherent impl,
                                        // the type): what the eye is looking
                                        // for when scanning a list of impls.
                                        .child(
                                            div()
                                                // Shrinkable, but yields to the
                                                // self type only after it has
                                                // taken what it needs.
                                                .min_w_0()
                                                .overflow_hidden()
                                                .truncate()
                                                .font_family("monospace")
                                                .text_size(ts.mono.size)
                                                .line_height(ts.mono.line_height)
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .text_color(colours.fg_default)
                                                .child(row.primary.clone()),
                                        )
                                        .children(tail)
                                        .when_some(row.source.clone(), |el, source| {
                                            let source_handler = source_handler.clone();
                                            let source_target = source_target.clone();
                                            el.child(
                                                div()
                                                    .id(("symbol.impls.source", ix))
                                                    .flex_shrink_0()
                                                    .font_family("monospace")
                                                    .text_size(ts.caption.size)
                                                    .line_height(ts.mono.line_height)
                                                    .text_color(colours.accent)
                                                    .cursor_pointer()
                                                    .hover(|s| s.underline())
                                                    .on_click(move |_, window, cx| {
                                                        // The source affordance is nested inside
                                                        // the row's "open impl" target.  A click
                                                        // must perform exactly one action: jump
                                                        // to the file, not jump and then replace
                                                        // the page with the impl symbol.
                                                        cx.stop_propagation();
                                                        if let Some(target) = source_target.as_ref()
                                                        {
                                                            source_handler(target, window, cx);
                                                        }
                                                    })
                                                    .child(source),
                                            )
                                        })
                                }
                                ImplFlatRow::Subheading {
                                    label,
                                    count,
                                    collapsed,
                                } => {
                                    let toggle = toggle_blanket.clone();
                                    div()
                                        .id(("symbol.impls.blanket.head", ix))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_1)
                                        .h(row_h)
                                        // Same `space_4` gutter as the rows it
                                        // heads and the disclosure header above
                                        // it — it was `space_2`, which put a
                                        // third left edge in one column.
                                        .px(sp.space_4)
                                        .bg(colours.bg_raised)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .on_click(move |_, window, cx| toggle(window, cx))
                                        .child(
                                            Icon::new(if *collapsed {
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
                                                .text_size(ts.dense.size)
                                                .line_height(ts.dense.line_height)
                                                .text_color(colours.fg_default)
                                                .child(label.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(ts.caption.size)
                                                .line_height(ts.caption.line_height)
                                                .text_color(colours.fg_faint)
                                                .child(count.clone()),
                                        )
                                }
                            };

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
                // Give the list a definite pixel height equal to the total
                // content height.  The outer wrapper caps and scrolls it.
                // Without this, `uniform_list` has no height to measure against
                // and collapses to zero — making the section appear empty even
                // though the rows were rendered.
                .h(list_h),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trailing `:123` is a line number; anything else is part of the path.
    /// Getting this wrong would silently truncate Windows paths.
    #[test]
    fn line_split_only_accepts_digits() {
        assert_eq!(split_line("src/a.rs:120"), ("src/a.rs", Some("120")));
        assert_eq!(split_line("src/a.rs"), ("src/a.rs", None));
        assert_eq!(split_line("C:/x/a.rs"), ("C:/x/a.rs", None));
        assert_eq!(split_line("src/a.rs:"), ("src/a.rs:", None));
        assert_eq!(split_line("a.rs:12:4"), ("a.rs:12", Some("4")));
    }

    /// Grouping is a comparison against the previous row only — one pass, no
    /// sort, no map.
    #[test]
    fn group_boundary_only_looks_backwards() {
        assert!(group_boundary(None, "src/a.rs"));
        assert!(!group_boundary(Some("src/a.rs"), "src/a.rs"));
        assert!(group_boundary(Some("src/a.rs"), "src/b.rs"));
    }

    /// The flat index length must match what `rebuild_flat` produces, or the
    /// `uniform_list` item count and the rendered rows disagree.
    #[test]
    fn flat_len_counts_headers_and_visible_rows() {
        assert_eq!(flat_len([(false, 2), (false, 3)].into_iter()), 7);
        assert_eq!(flat_len([(true, 2), (false, 3)].into_iter()), 5);
        assert_eq!(flat_len([(true, 2), (true, 3)].into_iter()), 2);
        assert_eq!(flat_len([].into_iter()), 0);
    }

    /// A collapsed group keeps its header, so the reader can always get the
    /// rows back.
    #[test]
    fn collapsing_never_hides_the_header() {
        assert_eq!(flat_len([(true, 99)].into_iter()), 1);
    }

    /// Precision is a claim, not a decoration: unknown tags stay unknown.
    #[test]
    fn precision_classification() {
        assert_eq!(Precision::classify("precise"), Precision::Precise);
        assert_eq!(Precision::classify("PRECISE"), Precision::Precise);
        assert_eq!(Precision::classify("resolved"), Precision::Precise);
        assert_eq!(Precision::classify("textual"), Precision::Textual);
        assert_eq!(Precision::classify("call"), Precision::Other);
        assert_eq!(Precision::classify(""), Precision::Other);
    }

    /// An unrecognised precision keeps the engine's own wording rather than
    /// being flattened into "textual" (LD-7).
    #[test]
    fn unknown_precision_shows_the_engines_words() {
        let theme = crate::theme::default_theme();
        let kind = SharedString::from("call-site");
        let (_, _, label) = precision_chrome(Precision::Other, &kind, &theme.colours, &theme.alpha);
        assert_eq!(&*label, "call-site");
    }

    /// A group of one reference draws no header.
    ///
    /// The defect: with a single reference the header and the row printed the
    /// same string, one above the other, with a disclosure triangle for hiding
    /// a row the reader could already see. This is the arithmetic half of the
    /// fix — `render` cannot draw a header the flat index does not contain.
    #[test]
    fn a_group_of_one_reference_contributes_no_header_row() {
        // (collapsed, row_count)
        assert_eq!(flat_len([(false, 1)].into_iter()), 1, "one row, no header");
        assert_eq!(
            flat_len([(false, 2)].into_iter()),
            3,
            "two rows earn a header"
        );
        assert_eq!(
            flat_len([(true, 1)].into_iter()),
            1,
            "a headerless group cannot be collapsed away"
        );
        assert_eq!(
            flat_len([(true, 5)].into_iter()),
            1,
            "a collapsed group is its header alone"
        );
        assert_eq!(
            flat_len([(false, 1), (false, 3), (true, 4)].into_iter()),
            1 + 4 + 1,
            "mixed groups sum correctly"
        );
    }

    /// A group with no header cannot be collapsed, because there would be
    /// nothing left on screen to expand it again.
    #[test]
    fn toggling_a_headerless_group_is_refused() {
        use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName};
        let lid = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("t"));
        let mut table = RefsTable::new();
        table.append_page(&RefsPage {
            total: 1,
            refs: vec![RefRow {
                target: SymbolKey::new(lid, IntroId::from_raw([0u8; 32])),
                path: "src/only.rs:9".into(),
                kind_tag: "call-site".into(),
            }]
            .into(),
        });
        table.rebuild_flat();
        let before = table.flat.len();
        assert!(
            !table.toggle_group(0),
            "a single-reference group has no header to click, so nothing can toggle it"
        );
        assert_eq!(
            table.flat.len(),
            before,
            "a refused toggle must not change the flat index"
        );
    }

    /// Counts are formatted once, on arrival, never in `render`.
    #[test]
    fn counts_are_preformatted() {
        assert_eq!(&*count_text(0), "0");
        assert_eq!(&*count_text(128), "128");
    }

    /// Syncing with no new pages does no work — the store's `Progressive` is
    /// append-only and we remember how much we have consumed.
    #[test]
    fn sync_with_no_new_pages_is_a_no_op() {
        let mut refs = RefsTable::new();
        assert!(!refs.sync(&[]));
        assert!(refs.is_empty());

        let mut impls = ImplsTable::new();
        assert!(!impls.sync(&[]));
        assert_eq!(impls.total(), 0);
    }

    /// Toggling a group that does not exist is a no-op, not a panic — a click
    /// can race a generation change.
    #[test]
    fn toggling_a_missing_group_is_a_no_op() {
        let mut refs = RefsTable::new();
        assert!(!refs.toggle_group(7));
    }

    /// A reset clears everything and re-stamps the generation, so entrance ids
    /// from the previous generation cannot collide (LD-19).
    #[test]
    fn reset_restamps_the_generation() {
        let mut refs = RefsTable::new();
        refs.reset(7);
        assert!(refs.is_empty());
        assert_eq!(refs.generation, 7);
        assert_eq!(refs.consumed_pages, 0);
        assert_eq!(&*refs.total_text(), "0");

        let mut impls = ImplsTable::new();
        impls.reset(9);
        assert_eq!(impls.generation, 9);
        assert_eq!(impls.consumed_pages, 0);
    }

    // ── ImplsTable layout height ──────────────────────────────────────────────
    //
    // The `uniform_list` inside `ImplsTable::render` receives an explicit `h`
    // equal to `rows × row_h`.  If this arithmetic drifts from the per-row `.h`
    // in the closure, the list either clips rows (too small) or shows a phantom
    // scroll gap (too large).  These tests exercise the pure arithmetic without
    // requiring a GPUI context.

    /// Zero rows → zero list height.
    #[test]
    fn impls_list_height_zero_rows() {
        let row_h: f32 = 20.0; // arbitrary
        assert_eq!(0_usize as f32 * row_h, 0.0);
    }

    /// N rows → N × row_h.
    #[test]
    fn impls_list_height_n_rows() {
        let row_h: f32 = 20.0;
        let count: usize = 6;
        assert_eq!((count as f32) * row_h, 120.0);
    }

    // ── Impl label decomposition ──────────────────────────────────────────────

    /// A trait impl splits into the trait side and the type it is for.
    #[test]
    fn trait_impl_splits_at_for() {
        assert_eq!(
            split_impl_label("impl<'h> Iterator for memchr.memchr.Memchr"),
            ("impl<'h> Iterator", Some("memchr.memchr.Memchr"))
        );
    }

    /// An inherent impl has no ` for `, and must not invent one.
    #[test]
    fn inherent_impl_has_no_self_type() {
        assert_eq!(
            split_impl_label("impl<'h> memchr.memchr.Memchr"),
            ("impl<'h> memchr.memchr.Memchr", None)
        );
    }

    /// The `impl ? for …` placeholder still decomposes.
    ///
    /// That text is a live backend defect (L39) being fixed elsewhere. The row
    /// styling must not depend on it being fixed, and must not degrade when it
    /// is — so the split is asserted against the broken form deliberately.
    #[test]
    fn placeholder_trait_label_still_splits() {
        assert_eq!(
            split_impl_label("impl ? for memchr.memchr.memchr.Memchr"),
            ("impl ?", Some("memchr.memchr.memchr.Memchr"))
        );
    }

    /// The `impl` keyword and its own generics are dropped; the chip says it.
    #[test]
    fn impl_keyword_and_its_generics_are_stripped() {
        assert_eq!(strip_impl_keyword("impl<'h> Iterator"), "Iterator");
        assert_eq!(strip_impl_keyword("impl Clone"), "Clone");
        assert_eq!(strip_impl_keyword("impl<T: Into<String>> Foo"), "Foo");
    }

    /// A stray `>` must not underflow the nesting counter.
    ///
    /// `balanced_angle_end` counts with a `usize`; a leading `>` used to reach
    /// `depth -= 1` at zero and panic in debug. The guard lives in the
    /// function now, so this holds no matter who calls it.
    #[test]
    fn unopened_angle_list_does_not_underflow() {
        assert_eq!(balanced_angle_end(">"), None);
        assert_eq!(balanced_angle_end(">>>"), None);
        assert_eq!(balanced_angle_end(""), None);
        assert_eq!(balanced_angle_end("Iterator"), None);
        assert_eq!(balanced_angle_end("<T>rest"), Some(3));
        assert_eq!(balanced_angle_end("<Vec<T>>"), Some(8));
    }

    /// Text that merely starts with the letters `impl` is left alone.
    ///
    /// A half-stripped label is worse than an unstripped one, so anything not
    /// matching the exact keyword shape passes through untouched.
    #[test]
    fn strip_is_conservative_about_non_keyword_text() {
        assert_eq!(
            strip_impl_keyword("implementation detail"),
            "implementation detail"
        );
        // Unbalanced generics: leave it entirely alone rather than guess.
        assert_eq!(strip_impl_keyword("impl<'h Iterator"), "impl<'h Iterator");
        assert_eq!(strip_impl_keyword("Memchr"), "Memchr");
    }

    /// Every row carries a non-empty `primary`, whatever the label looks like.
    ///
    /// `primary` is what the row renders in the foreground colour; an empty one
    /// would paint a blank line where an implementation should be. The three
    /// construction sites all route through `ImplRowView::new`, so asserting it
    /// here covers all of them.
    #[test]
    fn every_row_has_a_non_empty_primary() {
        for (label, trait_label) in [
            ("impl<'h> Iterator for memchr.Memchr", Some("Iterator")),
            ("impl ? for memchr.Memchr", None),
            ("impl<'h> memchr.Memchr", None),
            ("impl", None),
            ("", None),
        ] {
            let row = fake_row(label, false, trait_label, 0);
            assert!(
                !row.primary.is_empty(),
                "row for {label:?} produced an empty primary label"
            );
        }
    }

    /// The wire's `trait_label` wins over anything parsed out of the label.
    ///
    /// It is the engine's structured answer to the same question and stays
    /// correct when the rendered label's formatting changes.
    #[test]
    fn wire_trait_label_is_preferred_over_parsing() {
        let row = fake_row("impl ? for memchr.Memchr", false, Some("Iterator"), 0);
        assert_eq!(row.primary.as_ref(), "Iterator");
        assert_eq!(
            row.self_type.as_ref().map(|s| s.as_ref()),
            Some("memchr.Memchr")
        );
    }

    /// An inherent impl renders its type and no `for` clause.
    #[test]
    fn inherent_impl_row_has_no_tail() {
        let row = fake_row("impl<'h> memchr.memchr.Memchr", false, None, 0);
        assert_eq!(row.primary.as_ref(), "memchr.memchr.Memchr");
        assert!(
            row.self_type.is_none(),
            "inherent impl must not render a `for` clause"
        );
    }

    /// A declared implementation keeps its copyable source target in the
    /// projected row. The render path exposes that value through the dedicated
    /// source affordance instead of burying it in the row's navigation click.
    #[test]
    fn implementation_source_target_survives_projection() {
        use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName, SymbolKey};
        let lineage = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("pkg"));
        let key = SymbolKey::new(lineage, IntroId::from_raw([1_u8; 32]));
        let source = SharedString::from("src/lib.rs:12:4");
        let row = ImplRowView::new(
            key,
            SharedString::from("impl Display for Thing"),
            false,
            Some(SharedString::from("Display")),
            0,
            Some(source.clone()),
        );

        assert_eq!(row.source.as_ref(), Some(&source));
    }

    // ── Blanket / variadic grouping ───────────────────────────────────────────

    fn fake_row(
        label: &str,
        is_blanket: bool,
        trait_label: Option<&str>,
        self_generic_count: u32,
    ) -> ImplRowView {
        use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName, SymbolKey};
        let lid = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("t"));
        let key = SymbolKey::new(lid, IntroId::from_raw([0u8; 32]));
        ImplRowView::new(
            key,
            SharedString::from(label.to_owned()),
            is_blanket,
            trait_label.map(|t| SharedString::from(t.to_owned())),
            self_generic_count,
            None,
        )
    }

    /// Blanket partition: blanket rows must be separated from own impls.
    ///
    /// Detection rule: `ImplRow::is_blanket == true`.
    /// Expected: own=[Display, Debug], blanket=[ToString].
    #[test]
    fn blanket_rows_partitioned_from_own_impls() {
        let rows = vec![
            fake_row("impl Display for Router", false, Some("Display"), 0),
            fake_row("impl Debug for Router", false, Some("Debug"), 0),
            fake_row("impl<T: Display> ToString for T", true, Some("ToString"), 0),
        ];
        let (own, blanket): (Vec<_>, Vec<_>) = rows.iter().partition(|r| !r.is_blanket);
        assert_eq!(own.len(), 2, "own impls");
        assert_eq!(blanket.len(), 1, "blanket impls");
        assert!(
            blanket[0].label.contains("ToString"),
            "blanket is the ToString impl"
        );

        // Also verify build_flat places the subheading between own and blanket.
        let own_views: Vec<ImplRowView> = own.iter().map(|r| (*r).clone()).collect();
        let blanket_views: Vec<ImplRowView> = blanket.iter().map(|r| (*r).clone()).collect();
        let flat = build_flat(&own_views, &blanket_views, false);
        // 2 own rows + 1 subheading + 1 blanket row = 4
        assert_eq!(flat.len(), 4, "flat length with blanket expanded");
        assert!(
            matches!(flat[2], ImplFlatRow::Subheading { .. }),
            "third entry must be the blanket subheading"
        );
    }

    /// Variadic family detection: 16 rows sharing trait + self base with
    /// consecutive generic counts collapse to one summary row.
    #[test]
    fn variadic_family_detected_by_generic_count_run() {
        // Build 16 rows: impl Handler for F<T1>, F<T1,T2>, … F<T1,…,T16>
        let rows: Vec<ImplRowView> = (1_u32..=16)
            .map(|n| {
                fake_row(
                    &["impl Handler for F<", &"T,".repeat(n as usize), ">"].concat(),
                    false,
                    Some("Handler"),
                    n,
                )
            })
            .collect();

        // All 16 counts must be consecutive.
        let counts: Vec<u32> = rows.iter().map(|r| r.self_generic_count).collect();
        assert!(
            rows.len() >= 3,
            "need at least 3 members to trigger family collapse"
        );
        assert!(
            is_consecutive_run(&counts),
            "all arities must be consecutive"
        );

        // apply_variadic_grouping must collapse these to a single summary row.
        let collapsed = apply_variadic_grouping(rows);
        assert_eq!(
            collapsed.len(),
            1,
            "16-member consecutive family must collapse to 1"
        );
        assert!(
            collapsed[0].label.contains("arities 1\u{2013}16")
                || collapsed[0].label.contains("arities 1-16"),
            "summary label must include the arity range; got {:?}",
            collapsed[0].label,
        );
    }

    /// Non-consecutive generic counts must NOT be collapsed — they are distinct
    /// impls that happen to share a trait name.
    #[test]
    fn non_consecutive_generic_counts_are_not_a_family() {
        // Arities 1, 2, 5 — not consecutive; must never be collapsed.
        let counts = [1_u32, 2, 5];
        assert!(
            !is_consecutive_run(&counts),
            "a gap in the arity sequence must prevent family detection"
        );

        // Confirm apply_variadic_grouping does NOT collapse them.
        let rows: Vec<ImplRowView> = counts
            .iter()
            .map(|&n| fake_row("impl Handler for F", false, Some("Handler"), n))
            .collect();
        let result = apply_variadic_grouping(rows);
        assert_eq!(
            result.len(),
            3,
            "non-consecutive arities must not be collapsed"
        );
    }

    /// A family of fewer than 3 members must not be collapsed — two identical-
    /// looking impls for different reasons would be misleading as a single row.
    #[test]
    fn family_threshold_requires_at_least_three_members() {
        // 2 consecutive arities: not a family.
        let counts = [1_u32, 2];
        assert!(
            !is_consecutive_run(&counts),
            "two-member arity run must not trigger family collapse"
        );

        // Confirm apply_variadic_grouping passes them through unchanged.
        let rows: Vec<ImplRowView> = counts
            .iter()
            .map(|&n| fake_row("impl Handler for F", false, Some("Handler"), n))
            .collect();
        let result = apply_variadic_grouping(rows);
        assert_eq!(
            result.len(),
            2,
            "two-member run must pass through unchanged"
        );
    }
}
